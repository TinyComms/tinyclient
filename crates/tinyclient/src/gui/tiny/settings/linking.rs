//! Encryption's device linking: adding this device from one the person already uses, with their
//! messages, and adding a new device from this one (chatd's `e2ee/linking.rs` does the work).
//!
//! On the new device: a code, as text and as a QR of the same string, then six digits to read
//! out, then progress. On the device the person has: a field for the code (pasted; a camera is not
//! needed), then who is asking and its key's fingerprint, a field for the six digits the new device
//! shows, how much history to send with its size, and progress. And "New device added", on every
//! device of the person, with Revoke.

use super::encryption::Draft;
use super::kit::{self, card};
use crate::client::ask;
use crate::gui::components::{button, glyph, ButtonKind};
use crate::gui::signin::{code_tile, qr};
use crate::gui::state::Shell;
use crate::gui::theme::{self, Type};
use gpui::{div, prelude::*, px, ClipboardItem, Context, Div, SharedString, Window};
use protocol::{E2eeAsk, E2eeDevice, E2eeHistory, E2eeLink, E2eeStatus, LinkStage, Request, Response};
use std::rc::Rc;
use std::time::Duration;

/// How often progress is read again while history moves.
const PROGRESS: Duration = Duration::from_millis(600);

/// The history chosen when the person has not picked: everything this device read.
pub(in crate::gui) const DEFAULT_HISTORY: &str = "all";

/// "2.4 MB", "820 KB": about what a choice of history takes.
fn about(bytes: u64) -> String {
    let b = bytes as f64;
    if b >= 1e9 {
        format!("{:.1} GB", b / 1e9)
    } else if b >= 1e6 {
        format!("{:.1} MB", b / 1e6)
    } else {
        format!("{} KB", (b / 1e3).ceil().max(1.) as u64)
    }
}

/// What a choice of history is called.
fn history_words(choice: &E2eeHistory) -> String {
    let what = match choice.history.as_str() {
        "none" => "No messages".to_owned(),
        "all" => "Everything this device has".to_owned(),
        other => match other.strip_prefix("days:") {
            Some("1") => "The last day".to_owned(),
            Some(days) => format!("The last {days} days"),
            None => other.to_owned(),
        },
    };
    match (choice.history.as_str(), choice.messages) {
        ("none", _) => what,
        (_, 1) => format!("{what}: 1 message, about {}", about(choice.bytes)),
        (_, n) => format!("{what}: {n} messages, about {}", about(choice.bytes)),
    }
}

/// The digits in two groups of three, as both screens show them.
pub(in crate::gui) fn grouped_check(check: &str) -> String {
    if check.len() == 6 {
        format!("{} {}", &check[..3], &check[3..])
    } else {
        check.to_owned()
    }
}

/// What a link in progress says, in words.
pub(in crate::gui) fn stage_words(link: &E2eeLink) -> String {
    let new = link.role == "new";
    match link.stage {
        LinkStage::Waiting if new => "On a device you already use, open Encryption, choose Add a device, and scan or paste this code. It works for 5 minutes.".to_owned(),
        LinkStage::Waiting => "Waiting for the new device…".to_owned(),
        LinkStage::Confirm if new => "Your other device asks for these digits. Type them there.".to_owned(),
        LinkStage::Confirm => "Check that this is your device, then type the six digits it shows.".to_owned(),
        LinkStage::Sending => "Approving the new device and sending your messages…".to_owned(),
        LinkStage::Receiving => "Setting this device up and bringing your messages…".to_owned(),
        LinkStage::Done if new => match link.messages {
            0 => "This device is set up. It can read and send end-to-end encrypted messages from now on.".to_owned(),
            1 => "This device is set up, with 1 message from your other device.".to_owned(),
            n => format!("This device is set up, with {n} messages from your other device."),
        },
        LinkStage::Done => match link.left_out {
            0 => "Added. The new device can read and send from now on.".to_owned(),
            1 => "Added. The new device can read and send from now on. The server takes only so much at once, so 1 older message stayed on this device.".to_owned(),
            n => format!("Added. The new device can read and send from now on. The server takes only so much at once, so {n} older messages stayed on this device."),
        },
        LinkStage::Failed => link.error.clone(),
    }
}

fn progress(link: &E2eeLink) -> Option<String> {
    (link.total > 0 && matches!(link.stage, LinkStage::Sending | LinkStage::Receiving))
        .then(|| format!("{} of {}", about(link.done.min(link.total)), about(link.total)))
}

/// What a device of the notice is called.
fn device_name(device: &E2eeDevice) -> String {
    if device.label.is_empty() {
        "A device".to_owned()
    } else {
        device.label.clone()
    }
}

impl Shell {
    fn encryption_draft(&mut self) -> Option<&mut Draft> {
        self.tiny_state
            .settings
            .as_mut()
            .map(|overlay| &mut overlay.encryption)
    }

    /// Reads the link in progress; while history moves, again a moment later.
    pub(in crate::gui) fn tiny_e2ee_link_load(&mut self, cx: &mut Context<Self>) {
        let Some(account) = self.tiny_state.account.clone() else {
            return;
        };
        self.off_thread(
            cx,
            move || ask(&Request::TinyE2ee { account, ask: E2eeAsk::Link }),
            move |this, answer, cx| {
                let Ok(Response::TinyE2eeLink { link }) = answer else {
                    return;
                };
                let finished = link.as_ref().is_some_and(|link| link.stage == LinkStage::Done);
                let moving = link.as_ref().is_some_and(|link| matches!(link.stage, LinkStage::Sending | LinkStage::Receiving));
                let Some(draft) = this.encryption_draft() else {
                    return;
                };
                let was_done = draft.link.as_ref().is_some_and(|link| link.stage == LinkStage::Done);
                // The QR is made once per code.
                let code = link.as_ref().map(|link| link.code.clone()).unwrap_or_default();
                if draft.qr.as_ref().map(|(text, _)| text.as_str()) != Some(code.as_str()) {
                    draft.qr = (!code.is_empty()).then(|| qr::encode(code.as_bytes()).map(|made| (code.clone(), Rc::new(made)))).flatten();
                }
                draft.link = link;
                if finished && !was_done {
                    // The device's own state changed with it.
                    this.tiny_e2ee_load(cx);
                }
                if moving {
                    cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(PROGRESS).await;
                        let _ = this.update(cx, |this, cx| this.tiny_e2ee_link_load(cx));
                    })
                    .detach();
                }
                cx.notify();
            },
        );
    }

    /// One link ask; what it answers is in the next read of the link.
    fn tiny_e2ee_link_do(&mut self, ask_for: E2eeAsk, cx: &mut Context<Self>) {
        let Some(account) = self.tiny_state.account.clone() else {
            return;
        };
        if let Some(draft) = self.encryption_draft() {
            draft.error = None;
            draft.note = None;
        }
        cx.notify();
        self.off_thread(
            cx,
            move || ask(&Request::TinyE2ee { account, ask: ask_for }),
            move |this, answer, cx| {
                if let Some(draft) = this.encryption_draft() {
                    match answer {
                        Ok(Response::Ok) => {}
                        Ok(Response::Error { detail, .. }) => draft.error = Some(detail),
                        Ok(_) => draft.error = Some(crate::client::OTHER_ANSWER.to_owned()),
                        Err(err) => draft.error = Some(crate::client::trouble_words("tiny", &err)),
                    }
                }
                this.tiny_e2ee_link_load(cx);
                cx.notify();
            },
        );
    }

    fn tiny_e2ee_link_join(&mut self, cx: &mut Context<Self>) {
        let code = self.tiny_state.settings_fields.link_code.read(cx).text().trim().to_owned();
        if code.is_empty() {
            return;
        }
        self.tiny_state.settings_fields.link_code.update(cx, |input, cx| input.clear(cx));
        // A code from a device on a server without device linking is approved the old way, and
        // says so when it went through.
        if code.starts_with("tiny-device-") {
            self.tiny_e2ee_approve_code(code, cx);
            return;
        }
        self.tiny_e2ee_link_do(E2eeAsk::LinkJoin { code }, cx);
    }

    fn tiny_e2ee_link_confirm(&mut self, cx: &mut Context<Self>) {
        let check = self.tiny_state.settings_fields.link_check.read(cx).text().trim().to_owned();
        let history = self
            .encryption_draft()
            .map(|draft| draft.history.clone())
            .filter(|history| !history.is_empty())
            .unwrap_or_else(|| DEFAULT_HISTORY.to_owned());
        if check.chars().filter(char::is_ascii_digit).count() != 6 {
            if let Some(draft) = self.encryption_draft() {
                draft.error = Some("Type the six digits the new device shows.".to_owned());
            }
            cx.notify();
            return;
        }
        self.tiny_state.settings_fields.link_check.update(cx, |input, cx| input.clear(cx));
        self.tiny_e2ee_link_do(E2eeAsk::LinkConfirm { history, check }, cx);
    }

    fn tiny_e2ee_pick_history(&mut self, history: String, cx: &mut Context<Self>) {
        if let Some(draft) = self.encryption_draft() {
            draft.history = history;
        }
        cx.notify();
    }

    /// The person knows a device from the notice: it goes, here and on the Tiny page.
    pub(in crate::gui) fn tiny_e2ee_seen(&mut self, device_id: String, revoke: bool, cx: &mut Context<Self>) {
        let Some(account) = self.tiny_state.account.clone() else {
            return;
        };
        self.off_thread(
            cx,
            move || {
                if revoke {
                    ask(&Request::TinyE2ee { account, ask: E2eeAsk::Revoke { device_id } })
                } else {
                    ask(&Request::TinyE2ee { account, ask: E2eeAsk::SeenDevice { device_id } })
                }
            },
            move |this, answer, cx| {
                if let Ok(Response::Error { detail, .. }) = answer {
                    if let Some(draft) = this.encryption_draft() {
                        draft.error = Some(detail);
                    }
                }
                if let Some(account) = this.tiny_state.account.clone() {
                    this.fetch(account, None, cx);
                }
                if this.encryption_draft().is_some() {
                    this.tiny_e2ee_load(cx);
                }
                cx.notify();
            },
        );
    }
}

/// The words, the digits or the code, and the buttons of a link on show.
fn link_card(link: &E2eeLink, qr_code: Option<&Rc<qr::Qr>>, cx: &mut Context<Shell>) -> Div {
    let failed = link.stage == LinkStage::Failed;
    let done = link.stage == LinkStage::Done;
    let (icon, tone) = match link.stage {
        LinkStage::Failed => ("icons/triangle-alert.svg", theme::danger()),
        LinkStage::Done => ("icons/shield.svg", theme::positive()),
        _ => ("icons/key.svg", theme::text_soft()),
    };
    let mut body = card().p(px(12.)).gap(px(12.)).child(
        div()
            .flex()
            .items_start()
            .gap(px(10.))
            .child(glyph(icon, 16., tone))
            .child(div().flex_1().min_w_0().whitespace_normal().type_ui().text_color(theme::text()).child(stage_words(link))),
    );
    if link.role == "new" && link.stage == LinkStage::Waiting && !link.code.is_empty() {
        let copied = link.code.clone();
        let mut code = div().flex().flex_col().items_center().gap(px(10.));
        if let Some(made) = qr_code {
            code = code.child(code_tile(made.clone()));
        }
        body = body.child(code).child(
            div()
                .whitespace_normal()
                .type_small()
                .font_family(theme::MONO)
                .text_color(theme::text_soft())
                .child(link.code.clone()),
        );
        body = body.child(
            div().flex().gap(px(8.)).child(
                button("tiny-e2ee-link-copy", ButtonKind::Secondary, "Copy code")
                    .on_click(cx.listener(move |_, _, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(copied.clone())))),
            ),
        );
    }
    if link.role == "new" && link.stage == LinkStage::Confirm && !link.check.is_empty() {
        body = body.child(
            div()
                .flex()
                .justify_center()
                .py(px(8.))
                .text_size(px(36.))
                .line_height(px(44.))
                .font_family(theme::MONO)
                .font_weight(theme::SEMIBOLD)
                .text_color(theme::text())
                .child(grouped_check(&link.check)),
        );
    }
    if let Some(moved) = progress(link) {
        body = body.child(div().type_small().text_color(theme::text_faint()).child(moved));
    }
    let mut actions = div().flex().flex_wrap().gap(px(8.));
    if failed && link.role == "new" {
        actions = actions.child(
            button("tiny-e2ee-link-again", ButtonKind::Primary, "Try again")
                .on_click(cx.listener(|this, _, _, cx| this.tiny_e2ee_link_do(E2eeAsk::LinkStart, cx))),
        );
    } else if !failed && !done {
        actions = actions.child(
            button("tiny-e2ee-link-cancel", ButtonKind::Ghost, "Stop")
                .on_click(cx.listener(|this, _, _, cx| this.tiny_e2ee_link_do(E2eeAsk::LinkCancel, cx))),
        );
    }
    body.child(actions)
}

/// On a device waiting to be approved, on a server that links devices: the way to add it from a
/// device the person uses. `None` where the old paste-the-code approval is all there is.
pub(in crate::gui::tiny) fn new_device(status: &E2eeStatus, draft: &Draft, cx: &mut Context<Shell>) -> Option<Div> {
    if !status.linking || !matches!(status.device, protocol::DeviceState::AwaitingApproval { .. }) {
        return None;
    }
    let link = draft.link.as_ref().filter(|link| link.role == "new");
    let panel = match link {
        Some(link) => link_card(link, draft.qr.as_ref().map(|(_, made)| made), cx),
        None => card().p(px(12.)).gap(px(10.)).child(
            div().whitespace_normal().type_ui().text_color(theme::text()).child("Add this device from a device you already use. It shows a code there; your messages come with it."),
        )
        .child(
            div().flex().child(
                button("tiny-e2ee-link-start", ButtonKind::Primary, "Show a code")
                    .on_click(cx.listener(|this, _, _, cx| this.tiny_e2ee_link_do(E2eeAsk::LinkStart, cx))),
            ),
        ),
    };
    Some(panel)
}

/// On a device the person uses: adding a new one, from the code it shows.
pub(in crate::gui::tiny) fn add_device(shell: &Shell, status: &E2eeStatus, draft: &Draft, window: &mut Window, cx: &mut Context<Shell>) -> Div {
    let fields = &shell.tiny_state.settings_fields;
    let link = draft.link.as_ref().filter(|link| link.role == "old");
    let paste = |hint: &'static str, window: &mut Window, cx: &mut Context<Shell>| {
        div()
            .p(px(12.))
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(div().whitespace_normal().type_small().text_color(theme::text_soft()).child(hint))
            .child(
                div()
                    .flex()
                    .gap(px(8.))
                    .child(kit::input(&fields.link_code, None, window, cx).flex_1())
                    .child(button("tiny-e2ee-link-join", ButtonKind::Primary, "Continue").on_click(cx.listener(|this, _, _, cx| this.tiny_e2ee_link_join(cx)))),
            )
    };
    let Some(link) = link else {
        let hint = if status.linking {
            "Adding a device? On the new device, open Encryption and choose Show a code. Paste that code here."
        } else {
            "Setting up tinyclient on another computer? It shows a link code. Paste it here to approve that device. Check that its fingerprint matches what it shows."
        };
        return paste(hint, window, cx);
    };
    if link.stage != LinkStage::Confirm {
        let mut column = div().p(px(12.)).flex().flex_col().gap(px(8.)).child(link_card(link, None, cx));
        if matches!(link.stage, LinkStage::Done | LinkStage::Failed) {
            column = column.child(paste("Add another device: paste the code it shows.", window, cx));
        }
        return column;
    }
    // Confirm: who is asking, the digits, how much history.
    let who = match (link.label.is_empty(), link.os.is_empty()) {
        (false, false) => format!("Add {} ({}) to your account?", link.label, link.os),
        (false, true) => format!("Add {} to your account?", link.label),
        (true, false) => format!("Add a {} device to your account?", link.os),
        (true, true) => "Add this device to your account?".to_owned(),
    };
    let chosen = if draft.history.is_empty() { DEFAULT_HISTORY } else { draft.history.as_str() };
    let mut choices = div().flex().flex_col().gap(px(6.));
    for (ix, choice) in link.history.iter().enumerate() {
        let on = choice.history == chosen;
        let value = choice.history.clone();
        choices = choices.child(
            crate::gui::tiny::setup::pick_card(("tiny-e2ee-link-history", ix), on, true)
                .child(crate::gui::tiny::setup::radio(on))
                .child(div().flex_1().min_w_0().whitespace_normal().type_ui().text_color(theme::text()).child(history_words(choice)))
                .on_click(cx.listener(move |this, _, _, cx| this.tiny_e2ee_pick_history(value.clone(), cx))),
        );
    }
    card()
        .m(px(12.))
        .p(px(12.))
        .gap(px(12.))
        .child(div().whitespace_normal().type_ui().font_weight(theme::MEDIUM).text_color(theme::text()).child(who))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(div().type_small().text_color(theme::text_faint()).child("Its key fingerprint"))
                .child(
                    div()
                        .when(!crate::gui::phone::active(), |line| line.truncate())
                        .when(crate::gui::phone::active(), |line| line.whitespace_normal())
                        .type_small()
                        .font_family(theme::MONO)
                        .text_color(theme::text_soft())
                        .child(link.fingerprint.clone()),
                ),
        )
        .child(div().whitespace_normal().type_small().text_color(theme::text_soft()).child("If you didn't just start this on a device of yours, stop: someone may be trying to read your messages."))
        .child(div().type_small().text_color(theme::text_faint()).child("The six digits the new device shows"))
        .child(kit::input(&fields.link_check, Some(160.), window, cx))
        .child(div().type_small().text_color(theme::text_faint()).child("Messages to send it"))
        .child(choices)
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap(px(8.))
                .child(button("tiny-e2ee-link-confirm", ButtonKind::Primary, "Add device").on_click(cx.listener(|this, _, _, cx| this.tiny_e2ee_link_confirm(cx))))
                .child(button("tiny-e2ee-link-stop", ButtonKind::Ghost, "Stop").on_click(cx.listener(|this, _, _, cx| this.tiny_e2ee_link_do(E2eeAsk::LinkCancel, cx)))),
        )
}

/// "New device added": a device of the person that this one did not add, with Revoke.
pub(in crate::gui) fn added_notice(id: &'static str, added: &[E2eeDevice], cx: &mut Context<Shell>) -> Option<Div> {
    if added.is_empty() {
        return None;
    }
    let mut notice = div().flex().flex_col().gap(px(8.));
    for (ix, device) in added.iter().enumerate() {
        let since = protocol::tiny_unix(&device.created_at).map(crate::gui::time::date).unwrap_or_default();
        let words = if since.is_empty() {
            format!("New device added: {}. If it isn't yours, revoke it now.", device_name(device))
        } else {
            format!("New device added: {}, on {since}. If it isn't yours, revoke it now.", device_name(device))
        };
        let (revoke, mine) = (device.device_id.clone(), device.device_id.clone());
        notice = notice.child(
            div()
                .id(SharedString::from(format!("{id}-{ix}")))
                .p(px(12.))
                .rounded(px(theme::R_CARD))
                .bg(theme::raised())
                .border_1()
                .border_color(theme::warning())
                .flex()
                .flex_col()
                .gap(px(8.))
                .child(
                    div()
                        .flex()
                        .items_start()
                        .gap(px(10.))
                        .child(glyph("icons/triangle-alert.svg", 16., theme::warning()))
                        .child(div().flex_1().min_w_0().whitespace_normal().type_ui().text_color(theme::text()).child(words)),
                )
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap(px(8.))
                        .child(
                            button((SharedString::from(format!("{id}-revoke")), ix), ButtonKind::Danger, "Revoke")
                                .on_click(cx.listener(move |this, _, _, cx| this.tiny_e2ee_seen(revoke.clone(), true, cx))),
                        )
                        .child(
                            button((SharedString::from(format!("{id}-mine")), ix), ButtonKind::Ghost, "It's mine")
                                .on_click(cx.listener(move |this, _, _, cx| this.tiny_e2ee_seen(mine.clone(), false, cx))),
                        ),
                ),
        );
    }
    Some(notice)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_choices_read_with_their_size() {
        let choice = |history: &str, messages, bytes| E2eeHistory { history: history.into(), messages, bytes };
        assert_eq!(history_words(&choice("days:30", 120, 48_000)), "The last 30 days: 120 messages, about 48 KB");
        assert_eq!(history_words(&choice("all", 1, 900)), "Everything this device has: 1 message, about 1 KB");
        assert_eq!(history_words(&choice("none", 0, 256)), "No messages");
        assert_eq!(history_words(&choice("days:7", 3, 2_400_000)), "The last 7 days: 3 messages, about 2.4 MB");
    }

    #[test]
    fn the_digits_read_in_two_groups_and_each_stage_has_words() {
        assert_eq!(grouped_check("042117"), "042 117");
        assert_eq!(grouped_check(""), "");
        for role in ["new", "old"] {
            for stage in [LinkStage::Waiting, LinkStage::Confirm, LinkStage::Sending, LinkStage::Receiving, LinkStage::Done] {
                let link = E2eeLink { role: role.into(), stage, ..E2eeLink::default() };
                assert!(!stage_words(&link).is_empty());
            }
        }
        let failed = E2eeLink { stage: LinkStage::Failed, error: "The link ran out.".into(), ..E2eeLink::default() };
        assert_eq!(stage_words(&failed), "The link ran out.");
        let moving = E2eeLink { stage: LinkStage::Receiving, done: 500_000, total: 2_000_000, ..E2eeLink::default() };
        assert_eq!(progress(&moving).as_deref(), Some("500 KB of 2.0 MB"));
    }
}
