use super::{sheet, sheet_title};
use crate::gui::{components, Shell};
use crate::mobile_share::{self, Items};
use gpui::{div, prelude::*, Context, Div, Window};
use protocol::{Conversation, ConversationKind, Link, Request, Response};

#[derive(Default)]
pub(super) struct State {
    items: Items,
    rows: Option<Vec<Conversation>>,
}

impl Shell {
    pub(in crate::gui) fn receive_phone_share(&mut self, cx: &mut Context<Self>) {
        if self.phone_state.share.is_some() || crate::gui::lock::held()
            || self.snapshot.as_ref().is_none_or(|snapshot| snapshot.signed_in().is_empty())
        {
            return;
        }
        let Some(result) = mobile_share::next_share() else { return };
        let items = match result {
            Ok(items) => items,
            Err(message) => {
                self.toast(None, message, cx);
                return;
            }
        };
        self.phone_state.share = Some(State { items, rows: None });
        self.phone_state.share_epoch = self.phone_state.share_epoch.wrapping_add(1);
        let epoch = self.phone_state.share_epoch;
        self.off_thread(cx, || crate::client::ask(&Request::Find { network: None, query: String::new(), limit: protocol::PAGE_LIMIT }), move |this, response, cx| {
            if this.phone_state.share_epoch != epoch { return; }
            let Ok(Response::Conversations { conversations, .. }) = response else {
                if let Some(share) = this.phone_state.share.as_mut() {
                    share.rows = Some(Vec::new());
                }
                cx.notify();
                return;
            };
            let Some(snapshot) = this.snapshot.as_ref() else { return };
            let rows: Vec<_> = conversations.into_iter().filter(|row| {
                !row.read_only && !row.archived && !row.locked
                    && !matches!(row.kind, ConversationKind::Forum | ConversationKind::Voice)
                    && snapshot.account_link(&row.key) == Link::Connected
            }).collect();
            let Some(share) = this.phone_state.share.as_mut() else { return };
            let direct = mobile_share::direct_destination(&rows, share.items.destination.as_deref()).cloned();
            share.rows = Some(rows);
            if let Some(row) = direct {
                this.share_to(&row, cx);
            }
            cx.notify();
        });
        cx.notify();
    }

    fn share_to(&mut self, row: &Conversation, cx: &mut Context<Self>) {
        let Some(mut share) = self.phone_state.share.take() else { return };
        if let Err(items) = self.stage_phone_share(row, share.items, cx) {
            share.items = items;
            self.phone_state.share = Some(share);
            self.toast(None, "Choose a conversation that can send these items.", cx);
        } else {
            self.select(row.key.clone(), &row.title, cx);
        }
        cx.notify();
    }

    pub(super) fn dismiss_phone_share(&mut self, cx: &mut Context<Self>) -> bool {
        if self.phone_state.share.take().is_none() {
            return false;
        }
        cx.notify();
        true
    }

    pub(in crate::gui) fn phone_share_layer(&self, window: &Window, cx: &mut Context<Self>) -> Option<Div> {
        let share = self.phone_state.share.as_ref()?;
        let mut body = div().id("shared-items-dialog").role(gpui::Role::Dialog).aria_label("Share to conversation").aria_modal(true)
            .flex().flex_col().child(sheet_title("Share to conversation"))
            .child(components::button("cancel-share", components::ButtonKind::Ghost, "Cancel")
                .on_click(cx.listener(|this, _, _, cx| { this.dismiss_phone_share(cx); })));
        match &share.rows {
            None => body = body.child(div().child("Finding conversations…")),
            Some(rows) if rows.is_empty() => body = body.child(div().child("No conversations available.")),
            Some(rows) => {
                for (index, row) in rows.iter().enumerate() {
                    let row = row.clone();
                    let title = format!("{} · {}", row.title, row.key.network.label());
                    body = body.child(components::menu_item(("share-to", index), None, title, None)
                        .on_click(cx.listener(move |this, _, _, cx| this.share_to(&row, cx))));
                }
            }
        }
        Some(sheet("shared-items", f32::from(window.viewport_size().height), body,
            cx.listener(|this, _, _, cx| { this.dismiss_phone_share(cx); })))
    }
}
