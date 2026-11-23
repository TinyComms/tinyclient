//! Telegram live location sharing bar and platform location bridge (#452).
//!
//! Renders the persistent active-sharing bar listing every conversation currently receiving our
//! live location, with a per-chat **Stop** button and a **Stop all** button, and syncs mobile
//! platform foreground/background location tracking via [`protocol::location::provider`].

use super::components::{button, glyph, ButtonKind};
use super::state::Shell;
use super::theme::{self, Type};
use gpui::{div, prelude::*, px, AnyElement, Context, MouseButton, SharedString, Task};
use protocol::{AccountKey, Key, LiveLocationShare, Request, Response};
use std::collections::HashMap;
use std::time::Duration;

/// Live location sharing state owned by [`Shell`].
#[derive(Default)]
pub(super) struct State {
    /// Active live location shares grouped by account.
    pub by_account: HashMap<AccountKey, Vec<LiveLocationShare>>,
    /// Periodic ticker that refreshes remaining countdown labels and feeds GPS updates from
    /// the registered mobile [`protocol::location::LocationProvider`].
    pub ticker: Option<Task<()>>,
}

impl State {
    /// Flattened, deterministically ordered list of every active live location share across all
    /// signed-in accounts.
    pub fn all_shares(&self) -> Vec<LiveLocationShare> {
        let mut list: Vec<LiveLocationShare> = self
            .by_account
            .values()
            .flat_map(|v| v.iter().cloned())
            .collect();
        list.sort_by(|a, b| {
            b.started_at
                .cmp(&a.started_at)
                .then_with(|| a.conversation.id.cmp(&b.conversation.id))
                .then_with(|| a.message_id.cmp(&b.message_id))
        });
        list
    }

    /// Replaces the active share list for one account.
    pub fn replace_account(&mut self, account: AccountKey, shares: Vec<LiveLocationShare>) {
        if shares.is_empty() {
            self.by_account.remove(&account);
        } else {
            self.by_account.insert(account, shares);
        }
    }

    /// Removes one share optimistically when the user clicks Stop.
    pub fn remove_one(&mut self, conversation: &Key, message_id: &str) {
        let acct = conversation.account();
        if let Some(list) = self.by_account.get_mut(&acct) {
            list.retain(|s| !(s.conversation == *conversation && s.message_id == message_id));
            if list.is_empty() {
                self.by_account.remove(&acct);
            }
        }
    }

    /// Clears every share optimistically when the user clicks Stop all.
    pub fn clear_all(&mut self) {
        self.by_account.clear();
    }

    /// Returns a concise summary string for the mobile foreground-service notification.
    pub fn notification_summary(&self) -> String {
        let all = self.all_shares();
        match all.len() {
            0 => String::new(),
            1 => format!("Sharing live location with {}", all[0].conversation_title),
            n => format!("Sharing live location with {n} chats"),
        }
    }
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

impl Shell {
    /// Applies a `Change::LiveLocations` push from chatd and synchronizes mobile foreground
    /// location tracking.
    pub(super) fn live_locations_changed(
        &mut self,
        account: &AccountKey,
        shares: Vec<LiveLocationShare>,
        cx: &mut Context<Self>,
    ) {
        self.live_location_state.replace_account(account.clone(), shares);
        self.sync_location_provider(cx);
        cx.notify();
    }

    /// Fetches recovered active live shares across all accounts on startup / reconnect.
    pub(super) fn load_active_live_locations(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let answer = cx
                .background_executor()
                .spawn(async move {
                    crate::client::ask(&Request::ActiveLiveLocations { account: None })
                })
                .await;
            if let Ok(Response::LiveLocations { shares }) = answer {
                let _ = this.update(cx, |shell, cx| {
                    shell.live_location_state.by_account.clear();
                    for share in shares {
                        shell
                            .live_location_state
                            .by_account
                            .entry(share.conversation.account())
                            .or_default()
                            .push(share);
                    }
                    shell.sync_location_provider(cx);
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// Keeps the mobile platform's foreground location service and periodic GPS update ticker
    /// running if and only if at least one live share is active.
    fn sync_location_provider(&mut self, cx: &mut Context<Self>) {
        let all = self.live_location_state.all_shares();
        if let Some(provider) = protocol::location::provider() {
            if all.is_empty() {
                provider.stop_live_tracking();
            } else {
                let summary = self.live_location_state.notification_summary();
                let _ = provider.start_live_tracking(all.len(), &summary);
            }
        }
        if all.is_empty() {
            self.live_location_state.ticker = None;
            return;
        }
        if self.live_location_state.ticker.is_some() {
            return;
        }
        self.live_location_state.ticker = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(15)).await;
                let shares = this
                    .update(cx, |shell, cx| {
                        let list = shell.live_location_state.all_shares();
                        if list.is_empty() {
                            shell.live_location_state.ticker = None;
                        } else {
                            cx.notify();
                        }
                        list
                    })
                    .unwrap_or_default();
                if shares.is_empty() {
                    break;
                }
                // On mobile phones where a real LocationProvider is registered, push fresh GPS
                // coordinates to every active share (rate-limited inside the adapter).
                if let Some(provider) = protocol::location::provider() {
                    let fix = cx
                        .background_executor()
                        .spawn(async move { provider.current_location().ok() })
                        .await;
                    if let Some(point) = fix {
                        for share in shares {
                            let req = Request::UpdateLiveLocation {
                                conversation: share.conversation,
                                message_id: share.message_id,
                                point,
                                proximity_alert_radius: share.proximity_alert_radius,
                            };
                            let _ = cx
                                .background_executor()
                                .spawn(async move { crate::client::ask(&req) })
                                .await;
                        }
                    }
                }
            }
        }));
    }

    /// Stops one active live location share.
    pub(in crate::gui) fn stop_live_location(
        &mut self,
        conversation: Key,
        message_id: String,
        cx: &mut Context<Self>,
    ) {
        self.live_location_state.remove_one(&conversation, &message_id);
        self.sync_location_provider(cx);
        cx.notify();

        cx.spawn(async move |_, cx| {
            let _ = cx
                .background_executor()
                .spawn(async move {
                    crate::client::ask(&Request::StopLiveLocation {
                        conversation,
                        message_id,
                    })
                })
                .await;
        })
        .detach();
    }

    /// Stops every active live location share across all accounts.
    pub(in crate::gui) fn stop_all_live_locations(&mut self, cx: &mut Context<Self>) {
        self.live_location_state.clear_all();
        self.sync_location_provider(cx);
        cx.notify();

        cx.spawn(async move |_, cx| {
            let _ = cx
                .background_executor()
                .spawn(async move {
                    crate::client::ask(&Request::StopAllLiveLocations { account: None })
                })
                .await;
        })
        .detach();
    }

    /// Persistent active-sharing bar shown at the top of phone and desktop whenever one or more
    /// live locations are being shared. Lists every chat with a per-chat **Stop** control and a
    /// **Stop all** button.
    pub(in crate::gui) fn live_location_bar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let shares = self.live_location_state.all_shares();
        if shares.is_empty() {
            return None;
        }
        let now_s = unix_now();
        let count = shares.len();
        let heading = if count == 1 {
            "Sharing live location".to_owned()
        } else {
            format!("Sharing live location with {count} chats")
        };

        let items = shares.into_iter().enumerate().map(|(ix, share)| {
            let conv = share.conversation.clone();
            let msg_id = share.message_id.clone();
            let title = if share.conversation_title.is_empty() {
                share.conversation.id.clone()
            } else {
                share.conversation_title.clone()
            };
            let remain = share.status_label(now_s);
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .px(px(8.))
                .py(px(3.))
                .rounded(px(theme::R_CONTROL))
                .bg(theme::canvas())
                .border_1()
                .border_color(theme::hairline())
                .child(div().type_small().font_weight(theme::MEDIUM).text_color(theme::text()).child(title))
                .child(div().type_small().text_color(theme::text_faint()).child(remain))
                .child(
                    div()
                        .id(SharedString::from(format!("live-loc-stop-{ix}")))
                        .cursor_pointer()
                        .px(px(6.))
                        .py(px(1.))
                        .rounded(px(theme::R_CONTROL))
                        .bg(theme::danger_soft())
                        .type_small()
                        .font_weight(theme::MEDIUM)
                        .text_color(theme::danger())
                        .child("Stop")
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.stop_live_location(conv.clone(), msg_id.clone(), cx);
                        })),
                )
        });

        Some(
            div()
                .id("live-location-sharing-bar")
                .flex_none()
                .w_full()
                .px(px(12.))
                .py(px(6.))
                .bg(theme::accent_wash())
                .border_b_1()
                .border_color(theme::hairline())
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(8.))
                .child(glyph("icons/pin.svg", 14., theme::accent_text()))
                .child(
                    div()
                        .type_small()
                        .font_weight(theme::SEMIBOLD)
                        .text_color(theme::accent_text())
                        .child(heading),
                )
                .children(items)
                .child(div().flex_1())
                .child(
                    button("live-loc-stop-all", ButtonKind::Secondary, "Stop all")
                        .on_click(cx.listener(|this, _, _, cx| this.stop_all_live_locations(cx))),
                )
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::{GeoPoint, Network, LIVE_LOCATION_INDEFINITE};

    fn share(conv: &str, title: &str, msg_id: &str, period_secs: u32, started_at: i64) -> LiveLocationShare {
        LiveLocationShare {
            conversation: Key {
                network: Network::Telegram,
                account: "7001".into(),
                id: conv.into(),
            },
            conversation_title: title.into(),
            message_id: msg_id.into(),
            point: GeoPoint::from_deg(37.7749, -122.4194).unwrap(),
            started_at,
            period_secs,
            expires_at: (period_secs != LIVE_LOCATION_INDEFINITE).then_some(started_at + i64::from(period_secs)),
            proximity_alert_radius: None,
            last_updated_at: started_at,
        }
    }

    #[test]
    fn live_location_bar_state_tracks_multiple_chats_and_stops() {
        let mut state = State::default();
        let acct = AccountKey {
            network: Network::Telegram,
            account: "7001".into(),
        };
        let s1 = share("u7002", "Odalys", "101", 900, 1_000);
        let s2 = share("g8001", "Trail crew", "102", LIVE_LOCATION_INDEFINITE, 1_010);
        state.replace_account(acct.clone(), vec![s1.clone(), s2.clone()]);
        assert_eq!(state.all_shares().len(), 2);
        assert_eq!(state.notification_summary(), "Sharing live location with 2 chats");

        state.remove_one(&s1.conversation, &s1.message_id);
        assert_eq!(state.all_shares().len(), 1);
        assert_eq!(state.notification_summary(), "Sharing live location with Trail crew");

        state.clear_all();
        assert!(state.all_shares().is_empty());
        assert_eq!(state.notification_summary(), "");
    }
}
