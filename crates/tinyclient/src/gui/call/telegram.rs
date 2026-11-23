//! Telegram group/channel video chat controls (`#460` join-as identity picker & mid-call switch,
//! `#459` invite people, listener/speaker links, link reset, & `?videochat` link join prompt,
//! `#466` start confirmation dialog with title & RTMP toggle, rename, & leave vs end-for-everyone,
//! `#464` raise hand, roster ordering, participant bio, admin moderation, & `join_muted` toggle).

use super::bare_call;
use crate::client::ask;
use crate::gui::components::{
    button, glyph, menu_item, menu_item_danger, menu_label, menu_separator, modal_body,
    modal_buttons, scrim, Avatar, ButtonKind,
};
use crate::gui::input::TextInput;
use crate::gui::state::Shell;
use crate::gui::theme::{self, Type};
use gpui::{
    div, prelude::*, px, AnyElement, ClipboardItem, Context, Div, Entity, MouseButton,
    SharedString, Window,
};
use protocol::{
    Call, CallState, ConversationKind, GroupCallInviteResult, GroupCallJoinAs, GroupCallJoinPeer,
    Key, Network, Participant, Request, Response,
};
use std::cmp::Reverse;
use std::collections::HashSet;

/// Safe cap on people selected for a single Telegram video chat invite (`#459`).
pub(crate) const MAX_INVITE_SELECT: usize = 10;

/// What the mic control should do in a Telegram group video chat (`#464`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TelegramMicMode {
    Normal,
    RaiseHand,
    LowerHand,
}

/// Modal dialogs owned by Telegram group video chat controls.
pub(super) enum TelegramModal {
    /// Join-as identity picker (`#460`): choose "Me" or an eligible channel/group, optionally
    /// saving the choice for this chat.
    JoinAs {
        conversation: Key,
        invite_hash: Option<String>,
        peers: Vec<GroupCallJoinPeer>,
        selected: Option<String>,
        default_unknown: bool,
        remember: bool,
        switching: bool,
    },
    /// Confirmation dialog before starting a new video chat (`#466`), so an accidental click never
    /// notifies the whole group. Includes optional title and "Stream with another app" placeholder (#454).
    StartConfirm {
        conversation: Key,
        chat_title: String,
        is_channel: bool,
        title_input: Entity<TextInput>,
        rtmp_stream: bool,
    },
    /// Shown on leaving ONLY to users with `manage_call` (`#466`): Leave vs End for everyone.
    EndChoice {
        conversation: Key,
        chat_title: String,
    },
    /// Rename live video chat dialog (`#466`).
    Rename {
        conversation: Key,
        title_input: Entity<TextInput>,
    },
    /// Invite people picker (`#459`): explicit selection capped at [`MAX_INVITE_SELECT`],
    /// surfacing server per-person refusals.
    Invite {
        conversation: Key,
        candidates: Vec<protocol::Person>,
        selected: HashSet<String>,
        busy: bool,
        result: Option<GroupCallInviteResult>,
    },
    /// Explicit confirmation before removing a participant from a Telegram video chat (`#464`),
    /// making clear that removing them bans them from the group itself.
    RemoveConfirm {
        conversation: Key,
        participant_id: String,
        participant_name: String,
        chat_title: String,
    },
    /// Join prompt shown after opening a `t.me/<name>?videochat[=hash]`, `?voicechat`, or
    /// `?livestream` link inside tinyclient (`#459`).
    VideoChatLink {
        conversation: Key,
        chat_title: String,
        invite_hash: Option<String>,
    },
}

/// True when the join-as picker should be shown before calling `phone.joinGroupCall` (`#460`).
/// Skips the picker only when Telegram returned solely the user's personal account AND the
/// remembered default was readable.
pub(crate) fn join_as_needs_picker(info: &GroupCallJoinAs) -> bool {
    if info.default_unknown {
        return true;
    }
    if info.peers.len() > 1 {
        return true;
    }
    info.peers.first().is_some_and(|p| !p.is_self)
}

/// Initial selection in the join-as picker (`#460`). When `default_unknown` is true (full chat
/// could not be read), returns `None` so the user must explicitly choose rather than silently
/// defaulting to the personal account.
pub(crate) fn join_as_initial_selection(info: &GroupCallJoinAs) -> Option<String> {
    if info.default_unknown {
        return None;
    }
    if let Some(def) = info.default_peer.as_deref() {
        if info.peers.iter().any(|p| p.id == def) {
            return Some(def.to_owned());
        }
    }
    info.peers
        .iter()
        .find(|p| p.is_self)
        .or_else(|| info.peers.first())
        .map(|p| p.id.clone())
}

/// Sorts call participants for the roster (`#464`).
/// For Telegram: speakers first (active speakers before quiet speakers), then raised hands
/// (highest `raise_hand_rating` first), then listeners, then ringing entries.
/// Other networks keep alphabetical order after ringing.
pub(crate) fn sort_call_participants(participants: &[Participant], telegram: bool) -> Vec<&Participant> {
    let mut list: Vec<&Participant> = participants.iter().collect();
    if telegram {
        list.sort_by_key(|who| {
            let tier: u8 = if who.ringing {
                3
            } else if !who.suppressed {
                0
            } else if who.hand_raised {
                1
            } else {
                2
            };
            (
                tier,
                !who.speaking,
                who.muted,
                Reverse(who.raise_hand_rating.unwrap_or(0)),
                who.name.to_lowercase(),
            )
        });
    } else {
        list.sort_by_key(|who| (who.ringing, who.name.to_lowercase()));
    }
    list
}

/// What the mic button represents in a Telegram group call (`#464`).
pub(crate) fn telegram_mic_button_mode(call: &Call) -> TelegramMicMode {
    if call.conversation.network == Network::Telegram && call.suppressed {
        if call.hand_raised {
            TelegramMicMode::LowerHand
        } else {
            TelegramMicMode::RaiseHand
        }
    } else {
        TelegramMicMode::Normal
    }
}

/// Whether "End for everyone" may be offered on leave (`#466`).
pub(crate) fn can_end_call_for_everyone(call: &Call, manage_call: bool) -> bool {
    call.conversation.network == Network::Telegram
        && manage_call
        && matches!(call.state, CallState::Joined | CallState::Connecting | CallState::Reconnecting)
}

/// Admin moderation menu actions for one Telegram participant (`#464`). Empty without `manage_call`.
pub(crate) fn telegram_admin_participant_actions(
    call: &Call,
    participant: &Participant,
    manage_call: bool,
) -> Vec<&'static str> {
    if call.conversation.network != Network::Telegram || !manage_call {
        return Vec::new();
    }
    let mut actions = Vec::with_capacity(2);
    if participant.suppressed {
        actions.push("Allow to speak");
    } else {
        actions.push("Mute");
    }
    actions.push("Remove from group");
    actions
}

/// Display title for the call header (`#466`): includes `call.title` when set.
pub(crate) fn call_header_display_title(call: &Call, subject_title: &str) -> String {
    let custom = call.title.trim();
    if custom.is_empty() {
        subject_title.to_owned()
    } else if subject_title.is_empty() || subject_title == custom {
        custom.to_owned()
    } else {
        format!("{subject_title} · {custom}")
    }
}

impl Shell {
    pub(super) fn telegram_conversation_row(&self, key: &Key) -> Option<protocol::Conversation> {
        self.snapshot
            .as_ref()
            .and_then(|s| s.open.as_ref())
            .filter(|open| &open.key == key)
            .cloned()
            .or_else(|| self.sidebar_state.find(&self.trees, key))
    }

    pub(super) fn telegram_can_manage_call(&self, key: &Key) -> bool {
        key.network == Network::Telegram
            && self
                .telegram_conversation_row(key)
                .is_some_and(|row| row.rights.manage_call)
    }

    pub(super) fn telegram_is_group_or_channel(&self, key: &Key) -> bool {
        key.network == Network::Telegram
            && self
                .telegram_conversation_row(key)
                .is_some_and(|row| matches!(row.kind, ConversationKind::Group | ConversationKind::Channel))
    }

    pub(super) fn telegram_modal_close(&mut self, cx: &mut Context<Self>) {
        if self.call_state.telegram_modal.take().is_some() {
            cx.notify();
        }
    }

    /// Opens the start-video-chat confirmation dialog (`#466`). Never fires `createGroupCall` on a
    /// single click.
    pub(in crate::gui) fn telegram_open_start_confirm(&mut self, conversation: Key, cx: &mut Context<Self>) {
        let row = self.telegram_conversation_row(&conversation);
        if !row.as_ref().is_some_and(|r| r.rights.manage_call) {
            return;
        }
        let is_channel = row.as_ref().is_some_and(|r| r.kind == ConversationKind::Channel);
        let chat_title = row
            .map(|r| r.title)
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| "this chat".to_owned());
        let title_input = cx.new(|cx| TextInput::new("Optional title…", cx));
        self.call_menu_set(None, cx);
        self.call_state.telegram_modal = Some(TelegramModal::StartConfirm {
            conversation,
            chat_title,
            is_channel,
            title_input,
            rtmp_stream: false,
        });
        cx.notify();
    }

    /// Queries `phone.getGroupCallJoinAs` before joining (or switching identity mid-call) (`#460`).
    /// Skips the picker if only the personal account is returned and the default was readable.
    pub(in crate::gui) fn telegram_join_group_call(
        &mut self,
        conversation: Key,
        invite_hash: Option<String>,
        switching: bool,
        cx: &mut Context<Self>,
    ) {
        let req = Request::GroupCallJoinAs { conversation: conversation.clone() };
        cx.spawn(async move |this, cx| {
            let answer = cx.background_executor().spawn(async move { ask(&req) }).await;
            let _ = this.update(cx, |this, cx| match answer {
                Ok(Response::GroupCallJoinAs(info)) => {
                    if !switching && !join_as_needs_picker(&info) {
                        this.telegram_commit_join(conversation, None, invite_hash, false, cx);
                    } else {
                        let selected = if switching {
                            this.call_now()
                                .and_then(|c| c.joined_as.clone())
                                .or_else(|| join_as_initial_selection(&info))
                        } else {
                            join_as_initial_selection(&info)
                        };
                        this.call_menu_set(None, cx);
                        this.call_state.telegram_modal = Some(TelegramModal::JoinAs {
                            conversation,
                            invite_hash,
                            peers: info.peers,
                            selected,
                            default_unknown: info.default_unknown,
                            remember: false,
                            switching,
                        });
                        cx.notify();
                    }
                }
                _ => {
                    // Fallback when getGroupCallJoinAs cannot be fetched: ask with personal account only
                    // if switching, otherwise join directly.
                    this.telegram_commit_join(conversation, None, invite_hash, false, cx);
                }
            });
        })
        .detach();
    }

    pub(super) fn telegram_commit_join(
        &mut self,
        conversation: Key,
        join_as: Option<String>,
        invite_hash: Option<String>,
        save_default: bool,
        cx: &mut Context<Self>,
    ) {
        self.call_state.telegram_modal = None;
        self.call_state.connecting = Some(bare_call(conversation.clone(), CallState::Connecting, true, false));
        let req = Request::GroupCallJoin { conversation, join_as, invite_hash, save_default };
        cx.spawn(async move |this, cx| {
            let answer = cx.background_executor().spawn(async move { ask(&req) }).await;
            let _ = this.update(cx, |this, cx| {
                this.call_state.connecting = None;
                if let Ok(Response::Error { detail, .. }) = answer {
                    this.call_state.error = Some(detail);
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Offers Join after opening a `t.me/<name>?videochat[=hash]`, `?voicechat`, or `?livestream` link (`#459`).
    pub(in crate::gui) fn telegram_offer_videochat_link(
        &mut self,
        conversation: Key,
        chat_title: String,
        invite_hash: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let title = if chat_title.is_empty() {
            self.telegram_conversation_row(&conversation)
                .map(|r| r.title)
                .unwrap_or_else(|| "Video chat".to_owned())
        } else {
            chat_title
        };
        self.call_state.telegram_modal = Some(TelegramModal::VideoChatLink {
            conversation,
            chat_title: title,
            invite_hash,
        });
        cx.notify();
    }

    /// Handles Leave in a Telegram group call: admins with `manage_call` get a choice between
    /// "Leave" and "End for everyone"; everyone else leaves immediately (`#466`).
    pub(super) fn telegram_leave_or_end(&mut self, cx: &mut Context<Self>) {
        let end_target = self.call_now().and_then(|call| {
            let manage = self.telegram_can_manage_call(&call.conversation);
            can_end_call_for_everyone(call, manage).then(|| call.conversation.clone())
        });
        if let Some(conversation) = end_target {
            let chat_title = self
                .telegram_conversation_row(&conversation)
                .map(|r| r.title)
                .filter(|t| !t.is_empty())
                .unwrap_or_else(|| "this video chat".to_owned());
            self.call_menu_set(None, cx);
            self.call_state.telegram_modal = Some(TelegramModal::EndChoice {
                conversation,
                chat_title,
            });
            cx.notify();
        } else {
            self.call_leave(cx);
        }
    }

    /// Toggles raised hand when suppressed in a Telegram video chat (`#464`).
    pub(super) fn telegram_toggle_hand(&mut self, cx: &mut Context<Self>) {
        let Some((conversation, raised)) = self.call_now().and_then(|call| {
            (call.conversation.network == Network::Telegram && call.suppressed)
                .then(|| (call.conversation.clone(), !call.hand_raised))
        }) else {
            return;
        };
        self.call_ask(Request::GroupCallRaiseHand { conversation, raised }, cx);
    }

    pub(super) fn telegram_open_rename(&mut self, conversation: Key, current_title: String, cx: &mut Context<Self>) {
        let title_input = cx.new(|cx| {
            let mut input = TextInput::new("Video chat title", cx);
            if !current_title.is_empty() {
                input.set_text(&current_title, cx);
            }
            input
        });
        self.call_menu_set(None, cx);
        self.call_state.telegram_modal = Some(TelegramModal::Rename { conversation, title_input });
        cx.notify();
    }

    pub(super) fn telegram_open_invite(&mut self, conversation: Key, cx: &mut Context<Self>) {
        let account = conversation.account();
        let in_call: HashSet<String> = self
            .call_now()
            .map(|c| c.participants.iter().map(|p| p.id.clone()).collect())
            .unwrap_or_default();
        let me = self
            .snapshot
            .as_ref()
            .and_then(|s| s.accounts.iter().find(|a| a.key == account))
            .map(|a| a.me.clone())
            .unwrap_or_default();
        let mut candidates: Vec<protocol::Person> = self
            .people_state
            .hub()
            .friends(&account)
            .into_iter()
            .filter(|p| !p.bot && p.key.id != me && !in_call.contains(&p.key.id))
            .collect();
        candidates.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        self.call_menu_set(None, cx);
        self.call_state.telegram_modal = Some(TelegramModal::Invite {
            conversation,
            candidates,
            selected: HashSet::new(),
            busy: false,
            result: None,
        });
        cx.notify();
    }

    pub(super) fn telegram_export_link(&mut self, conversation: Key, speaker: bool, cx: &mut Context<Self>) {
        self.call_menu_close(cx);
        let req = Request::GroupCallExportInvite { conversation, can_self_unmute: speaker };
        cx.spawn(async move |this, cx| {
            let answer = cx.background_executor().spawn(async move { ask(&req) }).await;
            let _ = this.update(cx, |this, cx| match answer {
                Ok(Response::GroupCallInviteLink { link, speaker }) => {
                    cx.write_to_clipboard(ClipboardItem::new_string(link));
                    let label = if speaker {
                        "Copied speaker link to clipboard"
                    } else {
                        "Copied listener link to clipboard"
                    };
                    this.call_state.notice = Some(label.to_owned());
                    this.call_briefly(cx);
                }
                Ok(Response::Error { detail, .. }) => {
                    this.call_state.error = Some(detail);
                    cx.notify();
                }
                _ => {}
            });
        })
        .detach();
    }

    pub(super) fn telegram_reset_links(&mut self, conversation: Key, cx: &mut Context<Self>) {
        self.call_menu_close(cx);
        let req = Request::GroupCallSettings {
            conversation,
            join_muted: None,
            reset_invite_hash: true,
        };
        cx.spawn(async move |this, cx| {
            let answer = cx.background_executor().spawn(async move { ask(&req) }).await;
            let _ = this.update(cx, |this, cx| match answer {
                Ok(Response::Ok) => {
                    this.call_state.notice = Some("Video chat invite links reset".to_owned());
                    this.call_briefly(cx);
                }
                Ok(Response::Error { detail, .. }) => {
                    this.call_state.error = Some(detail);
                    cx.notify();
                }
                _ => {}
            });
        })
        .detach();
    }

    pub(super) fn telegram_toggle_join_muted(&mut self, conversation: Key, join_muted: bool, cx: &mut Context<Self>) {
        self.call_menu_close(cx);
        self.call_ask(
            Request::GroupCallSettings {
                conversation,
                join_muted: Some(join_muted),
                reset_invite_hash: false,
            },
            cx,
        );
    }

    pub(super) fn telegram_moderate_mute(
        &mut self,
        conversation: Key,
        participant_id: String,
        muted: bool,
        cx: &mut Context<Self>,
    ) {
        self.call_menu_close(cx);
        self.call_ask(
            Request::GroupCallModerate {
                conversation,
                participant_id,
                muted: Some(muted),
                remove: false,
            },
            cx,
        );
    }

    pub(super) fn telegram_confirm_remove(
        &mut self,
        conversation: Key,
        participant_id: String,
        participant_name: String,
        cx: &mut Context<Self>,
    ) {
        let chat_title = self
            .telegram_conversation_row(&conversation)
            .map(|r| r.title)
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| "this group".to_owned());
        self.call_menu_close(cx);
        self.call_state.telegram_modal = Some(TelegramModal::RemoveConfirm {
            conversation,
            participant_id,
            participant_name,
            chat_title,
        });
        cx.notify();
    }

    /// Popover menu for Telegram group video chat controls (`#460`, `#459`, `#466`, `#464`).
    pub(super) fn telegram_call_menu(&self, call: &Call, cx: &mut Context<Self>) -> Div {
        let manage = self.telegram_can_manage_call(&call.conversation);
        let conv = call.conversation.clone();
        let mut items = div()
            .flex()
            .flex_col()
            .child(menu_label("Video chat"))
            .child(menu_separator().mx(px(8.)))
            .child(
                menu_item("tg-call-invite", Some("icons/user-plus.svg"), "Invite people…", None).on_click(
                    cx.listener({
                        let conv = conv.clone();
                        move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.telegram_open_invite(conv.clone(), cx);
                        }
                    }),
                ),
            )
            .child(
                menu_item("tg-call-copy-listener", Some("icons/link.svg"), "Copy listener link", None).on_click(
                    cx.listener({
                        let conv = conv.clone();
                        move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.telegram_export_link(conv.clone(), false, cx);
                        }
                    }),
                ),
            );
        if manage {
            items = items
                .child(
                    menu_item("tg-call-copy-speaker", Some("icons/megaphone.svg"), "Copy speaker link", None)
                        .on_click(cx.listener({
                            let conv = conv.clone();
                            move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.telegram_export_link(conv.clone(), true, cx);
                            }
                        })),
                )
                .child(
                    menu_item("tg-call-reset-links", Some("icons/refresh-cw.svg"), "Reset invite links", None)
                        .on_click(cx.listener({
                            let conv = conv.clone();
                            move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.telegram_reset_links(conv.clone(), cx);
                            }
                        })),
                );
        }
        items = items.child(menu_separator().mx(px(8.))).child(
            menu_item("tg-call-switch-identity", Some("icons/users.svg"), "Switch identity…", None).on_click(
                cx.listener({
                    let conv = conv.clone();
                    move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.telegram_join_group_call(conv.clone(), None, true, cx);
                    }
                }),
            ),
        );
        if manage {
            items = items.child(menu_separator().mx(px(8.))).child(
                menu_item("tg-call-rename", Some("icons/pencil.svg"), "Rename video chat…", None).on_click(
                    cx.listener({
                        let conv = conv.clone();
                        let cur = call.title.clone();
                        move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.telegram_open_rename(conv.clone(), cur.clone(), cx);
                        }
                    }),
                ),
            );
            if call.can_change_join_muted {
                let next_jm = !call.join_muted;
                let label = if call.join_muted {
                    "New participants are muted: On"
                } else {
                    "New participants are muted: Off"
                };
                items = items.child(
                    menu_item(
                        "tg-call-join-muted",
                        Some(if call.join_muted { "icons/mic-off.svg" } else { "icons/mic.svg" }),
                        label,
                        None,
                    )
                    .on_click(cx.listener({
                        let conv = conv.clone();
                        move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.telegram_toggle_join_muted(conv.clone(), next_jm, cx);
                        }
                    })),
                );
            }
            items = items.child(menu_separator().mx(px(8.))).child(
                menu_item_danger("tg-call-end-all", Some("icons/phone-off.svg"), "End for everyone").on_click(
                    cx.listener({
                        let conv = conv.clone();
                        move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.call_menu_close(cx);
                            this.call_ask(Request::GroupCallDiscard { conversation: conv.clone() }, cx);
                        }
                    }),
                ),
            );
        }
        items
    }

    /// Renders any active Telegram video chat modal dialog above the workspace.
    pub(in crate::gui) fn telegram_call_modal(&self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let modal = self.call_state.telegram_modal.as_ref()?;
        let size = window.viewport_size();
        let wide = if crate::gui::phone::active() {
            360.0_f32.min(f32::from(size.width) - 32.)
        } else {
            400.0
        };
        let panel = match modal {
            TelegramModal::JoinAs {
                conversation,
                invite_hash,
                peers,
                selected,
                default_unknown,
                remember,
                switching,
            } => {
                let conv = conversation.clone();
                let hash = invite_hash.clone();
                let picked = selected.clone();
                let save = *remember;
                let heading = if *switching { "Switch video chat identity" } else { "Join video chat as…" };
                let note = if *default_unknown {
                    "Couldn't read your saved preference for this chat. Choose who to join as."
                } else {
                    "Choose your personal account or one of your channels."
                };
                let mut rows = div().mt(px(12.)).flex().flex_col().gap(px(4.));
                for peer in peers {
                    let is_sel = selected.as_deref() == Some(peer.id.as_str());
                    let pid = peer.id.clone();
                    let pkey = Key::new(&conversation.account(), &peer.id);
                    let av = Avatar::new(&pkey, &peer.name, 26.).build();
                    rows = rows.child(
                        div()
                            .id(SharedString::from(format!("tg-join-as-{}", peer.id)))
                            .h(px(38.))
                            .px(px(10.))
                            .rounded(px(theme::R_CONTROL))
                            .bg(if is_sel { theme::accent_soft() } else { theme::raised() })
                            .border_1()
                            .border_color(if is_sel { theme::accent() } else { theme::hairline() })
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(TelegramModal::JoinAs { selected, .. }) =
                                    this.call_state.telegram_modal.as_mut()
                                {
                                    *selected = Some(pid.clone());
                                    cx.notify();
                                }
                            }))
                            .child(av)
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .type_ui()
                                    .text_color(theme::text())
                                    .child(peer.name.clone()),
                            )
                            .when(is_sel, |r| r.child(glyph("icons/check.svg", 14., theme::accent()))),
                    );
                }
                let remember_row = div()
                    .id("tg-join-as-remember")
                    .mt(px(10.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(TelegramModal::JoinAs { remember, .. }) =
                            this.call_state.telegram_modal.as_mut()
                        {
                            *remember = !*remember;
                            cx.notify();
                        }
                    }))
                    .child(
                        div()
                            .size(px(16.))
                            .rounded(px(4.))
                            .border_1()
                            .border_color(if save { theme::accent() } else { theme::border() })
                            .bg(if save { theme::accent() } else { theme::raised() })
                            .flex()
                            .items_center()
                            .justify_center()
                            .when(save, |box_div| box_div.child(glyph("icons/check.svg", 11., theme::on_accent()))),
                    )
                    .child(div().type_small().text_color(theme::text_soft()).child("Remember for this chat"));
                let can_join = picked.is_some();
                let join_label = if *switching { "Switch & rejoin" } else { "Join" };
                div()
                    .child(div().type_title().child(heading))
                    .child(modal_body(note))
                    .child(rows)
                    .child(remember_row)
                    .child(
                        modal_buttons()
                            .child(
                                button("tg-join-as-cancel", ButtonKind::Secondary, "Cancel")
                                    .on_click(cx.listener(|this, _, _, cx| this.telegram_modal_close(cx))),
                            )
                            .child(
                                button("tg-join-as-confirm", ButtonKind::Primary, join_label)
                                    .when(!can_join, |b| b.opacity(0.45))
                                    .when(can_join, |b| {
                                        b.on_click(cx.listener(move |this, _, _, cx| {
                                            this.telegram_commit_join(
                                                conv.clone(),
                                                picked.clone(),
                                                hash.clone(),
                                                save,
                                                cx,
                                            );
                                        }))
                                    }),
                            ),
                    )
            }
            TelegramModal::StartConfirm {
                conversation,
                chat_title,
                is_channel,
                title_input,
                rtmp_stream,
            } => {
                let kind = if *is_channel { "live stream" } else { "video chat" };
                let rtmp = *rtmp_stream;
                let conv = conversation.clone();
                let input_ref = title_input.clone();
                let body = format!(
                    "Start a {kind} in {chat_title}? Members of this chat will be notified when it goes live."
                );
                let rtmp_row = div()
                    .id("tg-start-rtmp-toggle")
                    .mt(px(10.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(TelegramModal::StartConfirm { rtmp_stream, .. }) =
                            this.call_state.telegram_modal.as_mut()
                        {
                            *rtmp_stream = !*rtmp_stream;
                            cx.notify();
                        }
                    }))
                    .child(
                        div()
                            .size(px(16.))
                            .rounded(px(4.))
                            .border_1()
                            .border_color(if rtmp { theme::accent() } else { theme::border() })
                            .bg(if rtmp { theme::accent() } else { theme::raised() })
                            .flex()
                            .items_center()
                            .justify_center()
                            .when(rtmp, |b| b.child(glyph("icons/check.svg", 11., theme::on_accent()))),
                    )
                    .child(
                        div()
                            .type_small()
                            .text_color(theme::text_soft())
                            .child("Stream with another app (RTMP)"),
                    );
                div()
                    .child(div().type_title().child(if *is_channel { "Start live stream" } else { "Start video chat" }))
                    .child(modal_body(body))
                    .child(
                        div()
                            .mt(px(12.))
                            .px(px(10.))
                            .py(px(6.))
                            .rounded(px(theme::R_CONTROL))
                            .bg(theme::raised())
                            .border_1()
                            .border_color(theme::border())
                            .child(title_input.clone()),
                    )
                    .child(rtmp_row)
                    .child(
                        modal_buttons()
                            .child(
                                button("tg-start-cancel", ButtonKind::Secondary, "Cancel")
                                    .on_click(cx.listener(|this, _, _, cx| this.telegram_modal_close(cx))),
                            )
                            .child(
                                button("tg-start-confirm", ButtonKind::Primary, "Start").on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        let title = input_ref.read(cx).text().trim().to_owned();
                                        let title = (!title.is_empty()).then_some(title);
                                        let req = Request::GroupCallCreate {
                                            conversation: conv.clone(),
                                            title,
                                            rtmp_stream: rtmp,
                                            join: !rtmp,
                                        };
                                        this.telegram_modal_close(cx);
                                        let orig = conv.clone();
                                        cx.spawn(async move |this, cx| {
                                            let answer = cx.background_executor().spawn(async move { ask(&req) }).await;
                                            let _ = this.update(cx, |this, cx| match answer {
                                                Ok(Response::GroupCallStarted { conversation }) => {
                                                    if conversation != orig {
                                                        this.open_now(conversation, "", cx);
                                                    }
                                                }
                                                Ok(Response::Error { detail, .. }) => {
                                                    this.call_state.error = Some(detail);
                                                    cx.notify();
                                                }
                                                _ => {}
                                            });
                                        })
                                        .detach();
                                    },
                                )),
                            ),
                    )
            }
            TelegramModal::EndChoice { conversation, chat_title } => {
                let conv = conversation.clone();
                div()
                    .child(div().type_title().child("Leave video chat"))
                    .child(modal_body(format!(
                        "Leave {chat_title}, or end the video chat for everyone?"
                    )))
                    .child(
                        modal_buttons()
                            .child(
                                button("tg-end-cancel", ButtonKind::Secondary, "Cancel")
                                    .on_click(cx.listener(|this, _, _, cx| this.telegram_modal_close(cx))),
                            )
                            .child(
                                button("tg-end-leave-only", ButtonKind::Secondary, "Leave").on_click(
                                    cx.listener(|this, _, _, cx| {
                                        this.telegram_modal_close(cx);
                                        this.call_leave(cx);
                                    }),
                                ),
                            )
                            .child(
                                button("tg-end-everyone", ButtonKind::Danger, "End for everyone").on_click(
                                    cx.listener(move |this, _, _, cx| {
                                        this.telegram_modal_close(cx);
                                        this.call_state.leaving = true;
                                        this.call_ask(
                                            Request::GroupCallDiscard { conversation: conv.clone() },
                                            cx,
                                        );
                                    }),
                                ),
                            ),
                    )
            }
            TelegramModal::Rename { conversation, title_input } => {
                let conv = conversation.clone();
                let input_ref = title_input.clone();
                div()
                    .child(div().type_title().child("Rename video chat"))
                    .child(
                        div()
                            .mt(px(12.))
                            .px(px(10.))
                            .py(px(6.))
                            .rounded(px(theme::R_CONTROL))
                            .bg(theme::raised())
                            .border_1()
                            .border_color(theme::border())
                            .child(title_input.clone()),
                    )
                    .child(
                        modal_buttons()
                            .child(
                                button("tg-rename-cancel", ButtonKind::Secondary, "Cancel")
                                    .on_click(cx.listener(|this, _, _, cx| this.telegram_modal_close(cx))),
                            )
                            .child(
                                button("tg-rename-save", ButtonKind::Primary, "Save").on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        let title = input_ref.read(cx).text().trim().to_owned();
                                        this.telegram_modal_close(cx);
                                        this.call_ask(
                                            Request::GroupCallEditTitle {
                                                conversation: conv.clone(),
                                                title,
                                            },
                                            cx,
                                        );
                                    },
                                )),
                            ),
                    )
            }
            TelegramModal::Invite { conversation, candidates, selected, busy, result } => {
                let conv = conversation.clone();
                let chosen: Vec<String> = selected.iter().cloned().collect();
                let can_send = !chosen.is_empty() && !*busy;
                let mut list = div()
                    .id("tg-invite-candidates")
                    .mt(px(10.))
                    .max_h(px(220.))
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap(px(4.));
                if candidates.is_empty() {
                    list = list.child(
                        div()
                            .py(px(12.))
                            .type_small()
                            .text_color(theme::text_faint())
                            .child("No contacts available to invite."),
                    );
                } else {
                    for person in candidates {
                        let pid = person.key.id.clone();
                        let checked = selected.contains(&pid);
                        let av = Avatar::new(&person.key, &person.name, 22.).build();
                        list = list.child(
                            div()
                                .id(SharedString::from(format!("tg-inv-{}", person.key.id)))
                                .h(px(34.))
                                .px(px(8.))
                                .rounded(px(theme::R_CONTROL))
                                .bg(if checked { theme::accent_soft() } else { theme::raised() })
                                .flex()
                                .items_center()
                                .gap(px(8.))
                                .cursor_pointer()
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if let Some(TelegramModal::Invite { selected, .. }) =
                                        this.call_state.telegram_modal.as_mut()
                                    {
                                        if selected.contains(&pid) {
                                            selected.remove(&pid);
                                        } else if selected.len() < MAX_INVITE_SELECT {
                                            selected.insert(pid.clone());
                                        }
                                        cx.notify();
                                    }
                                }))
                                .child(av)
                                .child(div().flex_1().min_w_0().truncate().type_small().child(person.name.clone()))
                                .when(checked, |r| r.child(glyph("icons/check.svg", 13., theme::accent()))),
                        );
                    }
                }
                let refusal_block = result.as_ref().filter(|r| !r.refused.is_empty()).map(|res| {
                    let mut box_div = div()
                        .mt(px(8.))
                        .p(px(8.))
                        .rounded(px(theme::R_CONTROL))
                        .bg(theme::danger_soft())
                        .flex()
                        .flex_col()
                        .gap(px(4.));
                    for r in &res.refused {
                        box_div = box_div.child(
                            div()
                                .type_micro()
                                .text_color(theme::danger())
                                .child(format!("{}: {}", r.user_id, r.reason)),
                        );
                    }
                    box_div
                });
                div()
                    .child(div().type_title().child("Invite to video chat"))
                    .child(modal_body(format!(
                        "Select up to {MAX_INVITE_SELECT} people to invite."
                    )))
                    .child(list)
                    .children(refusal_block)
                    .child(
                        modal_buttons()
                            .child(
                                button("tg-invite-cancel", ButtonKind::Secondary, "Close")
                                    .on_click(cx.listener(|this, _, _, cx| this.telegram_modal_close(cx))),
                            )
                            .child(
                                button(
                                    "tg-invite-send",
                                    ButtonKind::Primary,
                                    if *busy { "Inviting…" } else { "Invite" },
                                )
                                .when(!can_send, |b| b.opacity(0.45))
                                .when(can_send, |b| {
                                    b.on_click(cx.listener(move |this, _, _, cx| {
                                        if let Some(TelegramModal::Invite { busy, .. }) =
                                            this.call_state.telegram_modal.as_mut()
                                        {
                                            *busy = true;
                                            cx.notify();
                                        }
                                        let req = Request::GroupCallInvite {
                                            conversation: conv.clone(),
                                            users: chosen.clone(),
                                        };
                                        cx.spawn(async move |this, cx| {
                                            let answer =
                                                cx.background_executor().spawn(async move { ask(&req) }).await;
                                            let _ = this.update(cx, |this, cx| match answer {
                                                Ok(Response::GroupCallInvited(res)) => {
                                                    if res.refused.is_empty() {
                                                        this.telegram_modal_close(cx);
                                                        this.call_state.notice =
                                                            Some(format!("Invited {} to the video chat", res.invited.len()));
                                                        this.call_briefly(cx);
                                                    } else if let Some(TelegramModal::Invite { busy, result, .. }) =
                                                        this.call_state.telegram_modal.as_mut()
                                                    {
                                                        *busy = false;
                                                        *result = Some(res);
                                                        cx.notify();
                                                    }
                                                }
                                                Ok(Response::Error { detail, .. }) => {
                                                    this.call_state.error = Some(detail);
                                                    if let Some(TelegramModal::Invite { busy, .. }) =
                                                        this.call_state.telegram_modal.as_mut()
                                                    {
                                                        *busy = false;
                                                    }
                                                    cx.notify();
                                                }
                                                _ => {}
                                            });
                                        })
                                        .detach();
                                    }))
                                }),
                            ),
                    )
            }
            TelegramModal::RemoveConfirm {
                conversation,
                participant_id,
                participant_name,
                chat_title,
            } => {
                let conv = conversation.clone();
                let pid = participant_id.clone();
                div()
                    .child(div().type_title().child(format!("Remove {participant_name} from group?")))
                    .child(modal_body(format!(
                        "Removing {participant_name} will ban them from {chat_title}, not just from this video chat."
                    )))
                    .child(
                        modal_buttons()
                            .child(
                                button("tg-remove-cancel", ButtonKind::Secondary, "Cancel")
                                    .on_click(cx.listener(|this, _, _, cx| this.telegram_modal_close(cx))),
                            )
                            .child(
                                button("tg-remove-confirm", ButtonKind::Danger, "Remove & ban from group")
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.telegram_modal_close(cx);
                                        this.call_ask(
                                            Request::GroupCallModerate {
                                                conversation: conv.clone(),
                                                participant_id: pid.clone(),
                                                muted: None,
                                                remove: true,
                                            },
                                            cx,
                                        );
                                    })),
                            ),
                    )
            }
            TelegramModal::VideoChatLink {
                conversation,
                chat_title,
                invite_hash,
            } => {
                let conv = conversation.clone();
                let hash = invite_hash.clone();
                div()
                    .child(div().type_title().child("Join video chat"))
                    .child(modal_body(format!(
                        "Join the video chat in {chat_title}?"
                    )))
                    .child(
                        modal_buttons()
                            .child(
                                button("tg-link-cancel", ButtonKind::Secondary, "Not now")
                                    .on_click(cx.listener(|this, _, _, cx| this.telegram_modal_close(cx))),
                            )
                            .child(
                                button("tg-link-join", ButtonKind::Primary, "Join").on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.telegram_modal_close(cx);
                                        this.telegram_join_group_call(conv.clone(), hash.clone(), false, cx);
                                    },
                                )),
                            ),
                    )
            }
        };

        let box_panel = div()
            .w(px(wide))
            .p(px(20.))
            .rounded(px(theme::R_MODAL))
            .bg(theme::overlay())
            .border_1()
            .border_color(theme::overlay_edge())
            .shadow(theme::shadow_modal())
            .font_family(theme::FONT)
            .text_color(theme::text())
            .occlude()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .flex()
            .flex_col()
            .child(panel);

        Some(
            div()
                .absolute()
                .top_0()
                .left_0()
                .w(size.width)
                .h(size.height)
                .child(scrim().id("tg-call-modal-scrim").on_click(cx.listener(|this, _, _, cx| {
                    this.telegram_modal_close(cx);
                })))
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .pb(px(60.))
                        .child(box_panel),
                )
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tg_call() -> Call {
        let key = Key { network: Network::Telegram, account: "7001".into(), id: "g8001".into() };
        bare_call(key, CallState::Joined, true, false)
    }

    #[test]
    fn join_as_skips_picker_only_when_only_self_and_default_known() {
        let only_self = GroupCallJoinAs {
            peers: vec![GroupCallJoinPeer { id: "u1".into(), name: "Me".into(), is_self: true, avatar: None }],
            default_peer: Some("u1".into()),
            default_unknown: false,
        };
        assert!(!join_as_needs_picker(&only_self));
        assert_eq!(join_as_initial_selection(&only_self), Some("u1".into()));

        // Multiple peers -> show picker and preselect remembered channel.
        let multi = GroupCallJoinAs {
            peers: vec![
                GroupCallJoinPeer { id: "u1".into(), name: "Me".into(), is_self: true, avatar: None },
                GroupCallJoinPeer { id: "c9001".into(), name: "Field Notes".into(), is_self: false, avatar: None },
            ],
            default_peer: Some("c9001".into()),
            default_unknown: false,
        };
        assert!(join_as_needs_picker(&multi));
        assert_eq!(join_as_initial_selection(&multi), Some("c9001".into()));

        // Unreadable full chat -> must ask and MUST NOT silently preselect personal account.
        let unknown = GroupCallJoinAs { default_unknown: true, default_peer: None, ..multi };
        assert!(join_as_needs_picker(&unknown));
        assert_eq!(join_as_initial_selection(&unknown), None);
    }

    #[test]
    fn roster_orders_speakers_then_raised_hands_then_listeners() {
        let listener = Participant {
            id: "u4".into(),
            name: "Zed".into(),
            muted: true,
            suppressed: true,
            ..Default::default()
        };
        let raised_low = Participant {
            id: "u3".into(),
            name: "Bea".into(),
            muted: true,
            suppressed: true,
            hand_raised: true,
            raise_hand_rating: Some(10),
            ..Default::default()
        };
        let raised_high = Participant {
            id: "u2".into(),
            name: "Cy".into(),
            muted: true,
            suppressed: true,
            hand_raised: true,
            raise_hand_rating: Some(500),
            ..Default::default()
        };
        let quiet_speaker = Participant {
            id: "c9".into(),
            name: "Field Notes".into(),
            muted: true,
            suppressed: false,
            is_channel: true,
            ..Default::default()
        };
        let active_speaker = Participant {
            id: "u1".into(),
            name: "Odalys".into(),
            muted: false,
            speaking: true,
            suppressed: false,
            ..Default::default()
        };
        let list = [listener, raised_low, raised_high, quiet_speaker, active_speaker];
        let ordered: Vec<&str> = sort_call_participants(&list, true).into_iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ordered, vec!["u1", "c9", "u2", "u3", "u4"]);
    }

    #[test]
    fn mic_mode_end_choice_header_title_and_admin_participant_menu() {
        let mut call = tg_call();
        assert_eq!(telegram_mic_button_mode(&call), TelegramMicMode::Normal);
        call.suppressed = true;
        assert_eq!(telegram_mic_button_mode(&call), TelegramMicMode::RaiseHand);
        call.hand_raised = true;
        assert_eq!(telegram_mic_button_mode(&call), TelegramMicMode::LowerHand);
        call.suppressed = false;
        assert_eq!(telegram_mic_button_mode(&call), TelegramMicMode::Normal);

        call.title = "Route planning".into();
        assert_eq!(call_header_display_title(&call, "Trail Crew"), "Trail Crew · Route planning");
        call.title.clear();
        assert_eq!(call_header_display_title(&call, "Trail Crew"), "Trail Crew");

        assert!(can_end_call_for_everyone(&call, true));
        assert!(!can_end_call_for_everyone(&call, false));

        let suppressed_p = Participant { id: "u2".into(), name: "Tobin".into(), suppressed: true, ..Default::default() };
        let speaker_p = Participant { id: "u1".into(), name: "Odalys".into(), suppressed: false, ..Default::default() };
        assert!(telegram_admin_participant_actions(&call, &suppressed_p, false).is_empty());
        assert_eq!(
            telegram_admin_participant_actions(&call, &suppressed_p, true),
            vec!["Allow to speak", "Remove from group"]
        );
        assert_eq!(
            telegram_admin_participant_actions(&call, &speaker_p, true),
            vec!["Mute", "Remove from group"]
        );
    }
}
