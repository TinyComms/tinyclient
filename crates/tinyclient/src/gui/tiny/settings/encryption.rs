//! Encryption: this device and the person's other devices, approving a new one, the people this
//! account shares encrypted conversations with (four emojis from each safety number, Telegram-
//! style), and the recovery key.
//!
//! It says what holds and no more. A device on the server's list that no device of its owner
//! signed is shown as not approved: the server may have added it, and nothing is encrypted to it.
//! A safety number marked as compared and since changed says so. The recovery key is shown once;
//! key backup is not built yet, and the page says that too.

use super::kit::{self, security_card as card};
use super::page_head;
use crate::client::ask;
use crate::gui::components::{button, button_with_icon, glyph, ButtonKind};
use crate::gui::state::Shell;
use crate::gui::theme::{self, Type};
use crate::gui::tiny::Page as Facts;
use gpui::{div, prelude::*, px, AnyElement, ClipboardItem, Context, Div, SharedString, Window};
use protocol::{
    grouped_safety, safety_emojis, DeviceState, E2eeAsk, E2eeDevice, E2eePerson, E2eeStatus, Request,
    Response, Trust,
};

/// What the page has read and what it is doing.
#[derive(Default)]
pub(in crate::gui) struct Draft {
    pub status: Option<E2eeStatus>,
    /// An explicit action or backup is running; background reads own separate request slots.
    pub loading: bool,
    pub asked: bool,
    pub error: Option<String>,
    /// What the last action said, when it went through.
    pub note: Option<String>,
    /// A recovery key just made: shown until the page closes.
    pub recovery: Option<String>,
    /// The person whose devices are open, fetched now.
    pub person: Option<E2eePerson>,
    /// The device link in progress on this device, as last read.
    pub link: Option<protocol::E2eeLink>,
    /// The QR of the link's code, made once per code.
    pub qr: Option<(String, std::rc::Rc<crate::gui::signin::qr::Qr>)>,
    /// The history picked to send a new device; empty is `linking::DEFAULT_HISTORY`.
    pub history: String,
    /// Fingerprints, compatibility codes and recovery information are shown on demand.
    pub details: bool,
    /// The encrypted conversation captured when its header opened this page.
    pub conversation: Option<protocol::Key>,
    /// Only one backup form is open at a time: save (`true`) or import (`false`).
    pub backup_mode: Option<bool>,
    scope: std::rc::Rc<()>,
    status_read: Option<std::rc::Rc<()>>,
    status_again: bool,
    link_read: Option<std::rc::Rc<()>>,
    link_again: bool,
}

/// The account and visible-page visit that own an asynchronous reply.
#[derive(Clone)]
pub(super) struct Visit {
    pub account: protocol::AccountKey,
    scope: std::rc::Rc<()>,
}

#[derive(Clone)]
pub(super) struct Read {
    pub visit: Visit,
    request: std::rc::Rc<()>,
}

fn begin_read(pending: &mut Option<std::rc::Rc<()>>, again: &mut bool) -> Option<std::rc::Rc<()>> {
    if pending.is_some() {
        *again = true;
        return None;
    }
    let request = std::rc::Rc::new(());
    *pending = Some(request.clone());
    Some(request)
}

fn finish_read(pending: &mut Option<std::rc::Rc<()>>, again: &mut bool, request: &std::rc::Rc<()>) -> Option<bool> {
    if !pending.as_ref().is_some_and(|current| std::rc::Rc::ptr_eq(current, request)) {
        return None;
    }
    *pending = None;
    Some(std::mem::take(again))
}

impl Draft {
    fn complete_repair(&mut self, visit: &Visit, account: Option<&protocol::AccountKey>, visible: bool, conversation: &protocol::Key, answer: std::io::Result<Response>) -> bool {
        if !self.accepts(visit, account, visible) || self.conversation.as_ref() != Some(conversation) { return false; }
        self.loading = false;
        match answer {
            Ok(Response::Ok) => {},
            Ok(Response::Error { detail, .. }) => self.error = Some(detail),
            Ok(_) => self.error = Some(crate::client::OTHER_ANSWER.to_owned()),
            Err(err) => self.error = Some(crate::client::trouble_words("tiny", &err)),
        }
        true
    }

    pub(super) fn visit(&self, account: &protocol::AccountKey) -> Visit {
        Visit { account: account.clone(), scope: self.scope.clone() }
    }

    pub(super) fn accepts(&self, visit: &Visit, account: Option<&protocol::AccountKey>, visible: bool) -> bool {
        visible && account == Some(&visit.account) && std::rc::Rc::ptr_eq(&self.scope, &visit.scope)
    }

    fn begin_status(&mut self, account: &protocol::AccountKey, visible: bool) -> Option<Read> {
        if !visible { return None; }
        let request = begin_read(&mut self.status_read, &mut self.status_again)?;
        self.asked = true;
        Some(Read { visit: self.visit(account), request })
    }

    fn status_changed(&mut self, change: &protocol::Change, account: &protocol::AccountKey, visible: bool) -> Option<Read> {
        if !matches!(change, protocol::Change::Tiny { account: changed } if changed == account) {
            return None;
        }
        self.begin_status(account, visible)
    }

    fn complete_status(&mut self, read: &Read, account: Option<&protocol::AccountKey>, visible: bool, answer: std::io::Result<Response>) -> Option<bool> {
        if !self.accepts(&read.visit, account, visible) { return None; }
        let again = finish_read(&mut self.status_read, &mut self.status_again, &read.request)?;
        match answer {
            Ok(Response::TinyE2ee { status }) => {
                if status.revoked {
                    self.link = None;
                    self.qr = None;
                    self.link_read = None;
                    self.link_again = false;
                }
                self.status = Some(status);
                self.error = None;
            }
            Ok(Response::Error { detail, .. }) => self.error = Some(detail),
            Ok(_) => self.error = Some(crate::client::OTHER_ANSWER.to_owned()),
            Err(err) => self.error = Some(crate::client::trouble_words("tiny", &err)),
        }
        Some(again)
    }

    pub(super) fn begin_link(&mut self, account: &protocol::AccountKey, visible: bool) -> Option<Read> {
        if !visible || self.status.as_ref().is_some_and(|status| status.revoked) { return None; }
        let request = begin_read(&mut self.link_read, &mut self.link_again)?;
        Some(Read { visit: self.visit(account), request })
    }

    pub(super) fn complete_link(&mut self, read: &Read, account: Option<&protocol::AccountKey>, visible: bool) -> Option<bool> {
        if !self.accepts(&read.visit, account, visible) { return None; }
        finish_read(&mut self.link_read, &mut self.link_again, &read.request)
    }

    pub(super) fn invalidate_reads(&mut self) {
        self.scope = std::rc::Rc::new(());
        self.status_read = None;
        self.status_again = false;
        self.link_read = None;
        self.link_again = false;
        self.loading = false;
    }
}

/// The words a device's state reads as.
pub(in crate::gui) fn state_words(state: &DeviceState) -> (&'static str, String) {
    match state {
        DeviceState::Off => ("icons/lock.svg", "This server doesn't offer end-to-end encrypted messages.".to_owned()),
        DeviceState::Ready => ("icons/shield.svg", "This device is ready for encrypted messages.".to_owned()),
        DeviceState::AwaitingApproval { .. } => (
            "icons/key.svg",
            "Approve this device from one you already use.".to_owned(),
        ),
        DeviceState::Failed { why } => ("icons/triangle-alert.svg", why.clone()),
    }
}

/// How a person's safety number stands, in words.
pub(in crate::gui) fn trust_words(person: &E2eePerson) -> &'static str {
    if person.identity_changed {
        return "New identity. Verify with them before accepting it.";
    }
    match person.trust {
        Trust::Verified => "Verified",
        Trust::Changed => "Changed since you verified it",
        Trust::Unverified => "Not verified",
    }
}

impl Shell {
    /// Opens Encryption from a conversation's header, when that conversation's server is the one
    /// the Tiny page holds.
    pub(in crate::gui) fn tiny_open_encryption(
        &mut self,
        conversation: &protocol::Key,
        encrypted: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let account = conversation.account();
        if self.tiny_state.account.as_ref() != Some(&account) || self.tiny_state.community.is_none()
        {
            return;
        }
        self.tiny_open_settings(super::Page::Encryption, None, window, cx);
        if let Some(overlay) = self.tiny_state.settings.as_mut() {
            overlay.encryption.conversation = encrypted.then(|| conversation.clone());
        }
        cx.notify();
    }

    pub(super) fn tiny_e2ee_visit(&self) -> Option<Visit> {
        let account = self.tiny_state.account.as_ref()?;
        let overlay = self.tiny_state.settings.as_ref()?;
        (overlay.page == super::Page::Encryption).then(|| overlay.encryption.visit(account))
    }

    pub(super) fn tiny_e2ee_draft_for(&mut self, visit: &Visit) -> Option<&mut Draft> {
        let account = self.tiny_state.account.as_ref();
        let overlay = self.tiny_state.settings.as_mut()?;
        overlay.encryption.accepts(visit, account, overlay.page == super::Page::Encryption)
            .then_some(&mut overlay.encryption)
    }

    pub(in crate::gui) fn tiny_e2ee_load(&mut self, cx: &mut Context<Self>) {
        let Some(account) = self.tiny_state.account.clone() else { return; };
        let Some(overlay) = self.tiny_state.settings.as_mut() else { return; };
        let Some(read) = overlay.encryption.begin_status(&account, overlay.page == super::Page::Encryption) else { return; };
        self.tiny_e2ee_status_read(read, cx);
    }

    pub(in crate::gui) fn tiny_e2ee_changed(&mut self, change: &protocol::Change, cx: &mut Context<Self>) {
        let Some(account) = self.tiny_state.account.clone() else { return; };
        let Some(overlay) = self.tiny_state.settings.as_mut() else { return; };
        let Some(read) = overlay.encryption.status_changed(change, &account, overlay.page == super::Page::Encryption) else { return; };
        self.tiny_e2ee_status_read(read, cx);
    }

    fn tiny_e2ee_status_read(&mut self, read: Read, cx: &mut Context<Self>) {
        let account = read.visit.account.clone();
        cx.notify();
        self.off_thread(
            cx,
            move || ask(&Request::TinyE2ee { account, ask: E2eeAsk::Status }),
            move |this, answer, cx| {
                let account = this.tiny_state.account.as_ref();
                let Some(overlay) = this.tiny_state.settings.as_mut() else { return; };
                let Some(again) = overlay.encryption.complete_status(&read, account, overlay.page == super::Page::Encryption, answer) else { return; };
                if again {
                    this.tiny_e2ee_load(cx);
                } else {
                    this.tiny_e2ee_link_load(cx);
                }
                cx.notify();
            },
        );
    }

    /// Sends one ask; on `Ok` says `done` and reads the page again.
    fn tiny_e2ee_do(&mut self, ask_for: E2eeAsk, done: &'static str, cx: &mut Context<Self>) {
        let Some(visit) = self.tiny_e2ee_visit() else { return; };
        let account = visit.account.clone();
        if let Some(draft) = self
            .tiny_state
            .settings
            .as_mut()
            .map(|overlay| &mut overlay.encryption)
        {
            draft.loading = true;
            draft.note = None;
            draft.error = None;
        }
        cx.notify();
        self.off_thread(
            cx,
            move || {
                ask(&Request::TinyE2ee {
                    account,
                    ask: ask_for,
                })
            },
            move |this, answer, cx| {
                let Some(draft) = this.tiny_e2ee_draft_for(&visit) else { return; };
                draft.loading = false;
                match answer {
                    Ok(Response::Ok) => draft.note = Some(done.to_owned()),
                    Ok(Response::TinyRecoveryKey { key }) => {
                        draft.recovery = Some(key);
                        draft.note = Some(done.to_owned());
                    }
                    Ok(Response::TinyE2eePerson { person }) => draft.person = Some(person),
                    Ok(Response::Error { detail, .. }) => draft.error = Some(detail),
                    Ok(_) => draft.error = Some(crate::client::OTHER_ANSWER.to_owned()),
                    Err(err) => draft.error = Some(crate::client::trouble_words("tiny", &err)),
                }
                this.tiny_e2ee_load(cx);
            },
        );
    }

    fn tiny_e2ee_repair(&mut self, cx: &mut Context<Self>) {
        let Some(visit) = self.tiny_e2ee_visit() else { return; };
        let Some(draft) = self.tiny_e2ee_draft_for(&visit) else { return; };
        let Some(conversation) = draft.conversation.clone() else { return; };
        if draft.loading || !draft.status.as_ref().is_some_and(|status| status.group_generations && status.device == DeviceState::Ready && !status.revoked) { return; }
        draft.loading = true;
        draft.error = None;
        draft.note = None;
        let account = visit.account.clone();
        let request_id = conversation.id.clone();
        self.off_thread(cx, move || ask(&Request::TinyE2ee { account, ask: E2eeAsk::Repair { conversation_id: request_id } }), move |this, answer, cx| {
            let account = this.tiny_state.account.as_ref();
            let Some(overlay) = this.tiny_state.settings.as_mut() else { return; };
            if !overlay.encryption.complete_repair(&visit, account, overlay.page == super::Page::Encryption, &conversation, answer) { return; }
            this.tiny_e2ee_load(cx);
            cx.notify();
        });
        cx.notify();
    }

    /// Approves a device from a `tiny-device-` code: the way before device linking, still how a
    /// device on a server without it is added.
    pub(in crate::gui::tiny) fn tiny_e2ee_approve_code(&mut self, code: String, cx: &mut Context<Self>) {
        self.tiny_e2ee_do(
            E2eeAsk::Approve { code },
            "Device approved.",
            cx,
        );
    }

    fn tiny_e2ee_check(&mut self, cx: &mut Context<Self>) {
        let key = self
            .tiny_state
            .settings_fields
            .recovery_check
            .read(cx)
            .text()
            .trim()
            .to_owned();
        if key.is_empty() {
            return;
        }
        self.tiny_state
            .settings_fields
            .recovery_check
            .update(cx, |input, cx| input.clear(cx));
        self.tiny_e2ee_do(
            E2eeAsk::CheckRecovery { key },
            "That is your recovery key.",
            cx,
        );
    }
}

fn repair_card(draft: &Draft, status: &E2eeStatus, cx: &mut Context<Shell>) -> Div {
    let repair = status.repair.as_ref().filter(|repair| draft.conversation.as_ref().is_some_and(|key| key.id == repair.conversation_id));
    let (words, busy, failed) = match repair.map(|repair| &repair.stage) {
        Some(protocol::RepairStage::Approving) => ("Approve on this device…", true, false),
        Some(protocol::RepairStage::Updating) => ("Updating encryption…", true, false),
        Some(protocol::RepairStage::WaitingForDevices) => ("Waiting for devices…", true, false),
        Some(protocol::RepairStage::Done) => ("Encryption updated. Saved messages are still available.", false, false),
        Some(protocol::RepairStage::Failed) => ("Saved messages are still available.", false, true),
        None => ("Saved messages stay available.", false, false),
    };
    let mut body = card().p(px(12.)).gap(px(10.))
        .child(kit::security_words(words).type_ui().text_color(theme::text_soft()));
    if failed {
        if let Some(repair) = repair { body = body.child(kit::note("icons/triangle-alert.svg", theme::danger(), kit::security_wrap(&repair.error))); }
    }
    if !busy {
        body = body.child(div().flex().min_w_0().child(button_with_icon(
            "tiny-e2ee-repair", ButtonKind::Secondary, "icons/shield.svg", "Update encryption",
        ).when(draft.loading, |button| button.opacity(0.5)).on_click(cx.listener(|this, _, _, cx| this.tiny_e2ee_repair(cx)))));
    }
    kit::security_section("icons/lock.svg", "This chat").child(body)
}

fn device_row(
    id: impl Into<SharedString>,
    device: &E2eeDevice,
    revoke: Option<gpui::Stateful<Div>>,
    details: bool,
) -> gpui::Stateful<Div> {
    let title = if device.this {
        "This device".to_owned()
    } else if device.label.is_empty() {
        "Device".to_owned()
    } else {
        device.label.clone()
    };
    let since = protocol::tiny_unix(&device.created_at)
        .map(crate::gui::time::date)
        .unwrap_or_default();
    let tone = if device.untrusted {
        theme::danger()
    } else {
        theme::text()
    };
    let mut column = div().flex_1().min_w_0().max_w_full().flex().flex_col().gap(px(4.))
        .child(kit::security_words(title).when(!details, |title| title.line_clamp(2)).type_ui().font_weight(theme::MEDIUM).text_color(tone))
        .when(details, |column| column.child(
            kit::security_words(&device.fingerprint).type_small().font_family(theme::MONO).text_color(theme::text_soft()),
        ))
        .when(device.untrusted, |column| column.child(
            kit::security_words("Unapproved. Cannot read your messages.").type_small().text_color(theme::danger()),
        ))
        .when(details && !since.is_empty(), |column| column.child(
            kit::security_words(format!("Added {since}")).type_small().text_color(theme::text_faint()),
        ));
    let phone = crate::gui::phone::active();
    let mut revoke = revoke;
    if phone {
        column = column.children(revoke.take().map(|action| div().flex().child(action)));
    }
    kit::table_row(id.into(), false)
        .items_start()
        .child(div().pt(px(2.)).child(glyph(
            if device.untrusted { "icons/triangle-alert.svg" } else { "icons/smartphone.svg" },
            16., tone,
        )))
        .child(column)
        .children(revoke)

}

fn security_details(open: bool, cx: &mut Context<Shell>) -> Div {
    div().flex().child(
        button_with_icon("tiny-e2ee-details", ButtonKind::Ghost,
            if open { "icons/chevron-up.svg" } else { "icons/chevron-down.svg" },
            if open { "Hide details" } else { "Security details" })
            .on_click(cx.listener(|this, _, _, cx| {
                if let Some(draft) = this.tiny_state.settings.as_mut().map(|overlay| &mut overlay.encryption) {
                    draft.details = !draft.details;
                }
                cx.notify();
            })),
    )
}

pub(in crate::gui::tiny) fn encryption(
    shell: &Shell,
    _facts: &Facts,
    window: &mut Window,
    cx: &mut Context<Shell>,
) -> AnyElement {
    let Some(draft) = shell
        .tiny_state
        .settings
        .as_ref()
        .map(|overlay| &overlay.encryption)
    else {
        return div().into_any_element();
    };
    let fields = &shell.tiny_state.settings_fields;
    let mut page = div().min_w_0().max_w_full().flex().flex_col().gap(px(24.))
        .when(!crate::gui::phone::active(), |page| page.child(page_head(
            "Encryption", "",
        )));
    if let Some(error) = &draft.error {
        page = page.child(kit::note(
            "icons/triangle-alert.svg",
            theme::danger(),
            kit::security_wrap(error),
        ));
    }
    if let Some(note) = &draft.note {
        page = page.child(kit::note(
            "icons/shield.svg",
            theme::positive(),
            kit::security_wrap(note),
        ));
    }
    let Some(status) = &draft.status else {
        let words = if draft.status_read.is_some() {
            "Reading this device's encryption…"
        } else {
            "Nothing read yet."
        };
        return page
            .child(div().type_ui().text_color(theme::text_soft()).child(words))
            .into_any_element();
    };

    let owner_link = draft.link.as_ref().is_some_and(|link| link.role == "old" && matches!(
        link.stage,
        protocol::LinkStage::Confirm | protocol::LinkStage::OwnerApproval |
        protocol::LinkStage::Sending | protocol::LinkStage::Receiving,
    ));
    if owner_link && status.device == DeviceState::Ready && !status.revoked {
        return page.child(
            kit::security_section("icons/smartphone.svg", "Add a device")
                .child(super::linking::add_device(shell, status, draft, window, cx))
                .child(security_details(draft.details, cx)),
        ).into_any_element();
    }

    // This device.
    let (icon, words) = if status.revoked {
        ("icons/triangle-alert.svg", "This device was removed. Saved messages are still available.".to_owned())
    } else {
        state_words(&status.device)
    };
    let state_tone = match (&status.device, status.revoked) {
        (_, true) | (DeviceState::Failed { .. }, _) => theme::danger(),
        (DeviceState::Ready, _) => theme::positive(),
        _ => theme::text_soft(),
    };
    let mut this = card().p(px(12.)).gap(px(10.)).child(
        div()
            .flex()
            .items_start()
            .gap(px(10.))
            .child(glyph(icon, 16., state_tone))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .whitespace_normal()
                    .type_ui()
                    .text_color(theme::text())
                    .child(kit::security_wrap(&words)),
            ),
    );
    if draft.details && !status.fingerprint.is_empty() {
        this = this
            .child(
                div()
                    .type_small()
                    .text_color(theme::text_faint())
                    .child("Key fingerprint"),
            )
            .child(
                kit::security_words(&status.fingerprint)
                    .type_small()
                    .font_family(theme::MONO)
                    .text_color(theme::text()),
            );
    }
    let new_device = super::linking::new_device(status, draft, cx);
    if let DeviceState::AwaitingApproval { code } = &status.device {
        if new_device.is_none() || draft.details {
            let copied = code.clone();
            this = this
                .child(
                    div()
                        .type_small()
                        .text_color(theme::text_faint())
                        .child(if new_device.is_some() {
                            "For an older device"
                        } else {
                            "Link code"
                        }),
                )

                .child(
                    div().flex().child(
                        button(
                            "tiny-e2ee-copy-code",
                            ButtonKind::Secondary,
                            "Copy link code",
                        )
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(copied.clone()))
                        })),
                    ),
                );
        }
    }
    let mut this_device = kit::security_section("icons/shield.svg", "This device");
    let linking = new_device.is_some();
    if let Some(panel) = new_device {
        this_device = this_device.child(panel);
    }
    if !linking || draft.details {
        this_device = this_device.child(this);
    }
    this_device = this_device.child(security_details(draft.details, cx));
    page = page.child(this_device);
    if status.device == DeviceState::Off {
        return page.into_any_element();
    }
    if status.revoked {
        // Saved history stays usable after removal, including its local encrypted backup.
        return page.child(super::backup::backup(shell, draft.loading, window, cx)).into_any_element();
    }
    if let Some(notice) = super::linking::added_notice("tiny-e2ee-added", &status.added, cx) {
        page = page.child(notice);
    }
    // Keep setup focused on the current step. Device and backup management remain available
    // in Security details until this device is approved.
    if matches!(status.device, DeviceState::AwaitingApproval { .. }) && !draft.details {
        return page.into_any_element();
    }

    if status.device == DeviceState::Ready && status.group_generations && draft.conversation.is_some() {
        page = page.child(repair_card(draft, status, cx));
    }

    // Your devices.
    let mut mine = card();
    if status.device == DeviceState::Ready {
        mine = mine.child(super::linking::add_device(shell, status, draft, window, cx));
    }
    for (ix, device) in status.devices.iter().enumerate() {
        let id = device.device_id.clone();
        let revoke = button(
            ("tiny-e2ee-revoke", ix),
            ButtonKind::Ghost,
            if device.this {
                "Remove this device"
            } else {
                "Revoke"
            },
        )
        .on_click(cx.listener(move |this, _, _, cx| {
            this.tiny_e2ee_do(
                E2eeAsk::Revoke {
                    device_id: id.clone(),
                },
                "Revoked. It can't read new messages.",
                cx,
            )
        }));
        mine = mine.child(device_row(
            SharedString::from(format!("tiny-e2ee-mine-{ix}")),
            device,
            Some(revoke),
            draft.details,
        ));
    }
    if status.devices.is_empty() {
        mine = mine.child(
            div()
                .p(px(12.))
                .type_ui()
                .text_color(theme::text_soft())
                .child("No device is registered yet."),
        );
    }
    page = page.child(kit::security_section("icons/smartphone.svg", "Your devices").child(mine));

    // People.
    let mut people = card();
    for (ix, person) in status.people.iter().enumerate() {
        let name = if person.name.is_empty() {
            "Someone".to_owned()
        } else {
            person.name.clone()
        };
        let user = person.user_id.clone();
        let verified = person.trust == Trust::Verified;
        let toggle = {
            let user = user.clone();
            button(
                ("tiny-e2ee-verify", ix),
                ButtonKind::Ghost,
                if verified {
                    "Unmark"
                } else {
                    "Mark as verified"
                },
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.tiny_e2ee_do(
                    E2eeAsk::Verify {
                        user_id: user.clone(),
                        verified: !verified,
                    },
                    if verified {
                        "No longer marked as verified."
                    } else {
                        "Marked as verified."
                    },
                    cx,
                )
            }))
        };
        let devices = {
            let user = user.clone();
            button(("tiny-e2ee-devices", ix), ButtonKind::Ghost, "Devices").on_click(cx.listener(
                move |this, _, _, cx| {
                    this.tiny_e2ee_do(
                        E2eeAsk::Person {
                            user_id: user.clone(),
                        },
                        "",
                        cx,
                    )
                },
            ))
        };
        let accept = person.identity_changed.then(|| {
            let user = user.clone();
            button(
                ("tiny-e2ee-accept", ix),
                ButtonKind::Secondary,
                "Accept new identity",
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.tiny_e2ee_do(
                    E2eeAsk::AcceptIdentity {
                        user_id: user.clone(),
                    },
                    "Accepted. Compare the new safety number with them.",
                    cx,
                )
            }))
        });
        let tone = match (person.identity_changed, person.trust) {
            (true, _) | (_, Trust::Changed) => theme::danger(),
            (_, Trust::Verified) => theme::positive(),
            _ => theme::text_soft(),
        };
        let emojis = safety_emojis(&person.safety);
        let emoji_row = if emojis.is_empty() {
            div()
                .type_small()
                .text_color(theme::text_faint())
                .child("No key to compare yet")
                .into_any_element()
        } else {
            div()
                .flex()
                .flex_wrap()
                .gap(px(6.))
                .children(emojis.into_iter().enumerate().map(|(ei, face)| {
                    div()
                        .id(SharedString::from(format!("tiny-e2ee-emoji-{ix}-{ei}")))
                        .size(px(36.))
                        .rounded(px(8.))
                        .bg(theme::hover())
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(px(20.))
                        .line_height(px(24.))
                        .child(face)
                }))
                .into_any_element()
        };
        people = people.child(
            kit::table_row(SharedString::from(format!("tiny-e2ee-person-{ix}")), false)
                .flex_col().items_start().gap(px(10.))
                .child(
                    div()
                        .w_full()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(6.))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .type_ui()
                                        .font_weight(theme::MEDIUM)
                                        .text_color(theme::text())
                                        .child(name),
                                )
                                .when(verified && !person.identity_changed && person.trust != Trust::Changed, |row| {
                                    row.child(glyph("icons/badge-check.svg", 14., theme::positive()))
                                }),
                        )
                        .child(emoji_row)
                        .when(draft.details && !person.safety.is_empty(), |column| {
                            column.child(
                                div()
                                    .whitespace_normal()
                                    .type_small()
                                    .font_family(theme::MONO)
                                    .text_color(theme::text_faint())
                                    .child(grouped_safety(&person.safety)),
                            )
                        })
                        .child(
                            div()
                                .whitespace_normal()
                                .type_small()
                                .text_color(tone)
                                .child(trust_words(person)),
                        )
                        .child(div().type_small().text_color(theme::text_faint()).child(
                            match person.devices.len() {
                                1 => "1 device".to_owned(),
                                n => format!("{n} devices"),
                            },
                        )),
                )
                .child(
                    div()
                        .flex_none()
                        .flex()
                        .max_w_full()
                        .flex_wrap()
                        .gap(px(4.))
                        .children(accept)
                        .child(devices)
                        .when(
                            !person.safety.is_empty() && !person.identity_changed,
                            |row| row.child(toggle),
                        ),
                ),
        );
    }
    if status.people.is_empty() {
        people = people.child(
            div()
                .p(px(12.))
                .type_ui()
                .text_color(theme::text_soft())
                .child("People you message will appear here."),
        );
    }
    page = page.child(
        kit::security_section("icons/users.svg", "People")
            .when(!status.people.is_empty(), |section| section.child(kit::security_words("Compare these emojis together to verify a conversation.").type_small().text_color(theme::text_soft())))
            .child(people),
    );
    if let Some(person) = &draft.person {
        let mut list = card();
        for (ix, device) in person.devices.iter().enumerate() {
            list = list.child(device_row(
                SharedString::from(format!("tiny-e2ee-theirs-{ix}")),
                device,
                None,
                true,
            ));
        }
        let name = if person.name.is_empty() {
            "Their devices".to_owned()
        } else {
            format!("{}'s devices", person.name)
        };
        page = page.child(
            div()
                .flex()
                .flex_col()
                .gap(px(8.))
                .child(
                    div()
                        .type_ui()
                        .font_weight(theme::MEDIUM)
                        .text_color(theme::text())
                        .child(name),
                )
                .child(list),
        );
    }

    // Recovery.
    let mut recovery = card().p(px(12.)).gap(px(10.)).child(div().whitespace_normal().type_small().text_color(theme::text_soft()).child(
        "Recovery keys do not restore messages yet. Use a backup file below to save your messages.",
    ));
    if let Some(key) = &draft.recovery {
        let copied = key.clone();
        recovery = recovery
            .child(
                kit::security_words(key)
                    .type_ui()
                    .font_family(theme::MONO)
                    .text_color(theme::text()),
            )
            .child(div().flex().child(
                button("tiny-e2ee-copy-recovery", ButtonKind::Secondary, "Copy").on_click(
                    cx.listener(move |_, _, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(copied.clone()))
                    }),
                ),
            ));
    }
    let make = if status.recovery_set {
        "Make a new recovery key"
    } else {
        "Make a recovery key"
    };
    recovery = recovery.child(div().flex().child(
        button("tiny-e2ee-recovery", ButtonKind::Secondary, make).on_click(cx.listener(
            |this, _, _, cx| {
                this.tiny_e2ee_do(
                    E2eeAsk::RecoveryKey,
                    "Recovery key made. It won't be shown again.",
                    cx,
                )
            },
        )),
    ));
    if status.recovery_set {
        recovery = recovery.child(
            div()
                .flex()
                .gap(px(8.))
                .child(kit::input(&fields.recovery_check, None, window, cx).flex_1().min_w_0().max_w_full())
                .child(
                    button("tiny-e2ee-check", ButtonKind::Ghost, "Check")
                        .on_click(cx.listener(|this, _, _, cx| this.tiny_e2ee_check(cx))),
                ),
        );
    }
    page.when(draft.details, |page| page.child(kit::security_section("icons/key.svg", "Recovery key").child(recovery)))
        .child(super::backup::backup(shell, draft.loading, window, cx))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(name: &str) -> protocol::AccountKey {
        protocol::AccountKey { network: protocol::Network::Tiny, account: name.into() }
    }

    fn ready() -> E2eeStatus {
        E2eeStatus { device: DeviceState::Ready, ..E2eeStatus::default() }
    }

    #[test]
    fn encryption_repair_completion_is_bound_to_account_visit_and_conversation() {
        let account = account("one");
        let conversation = protocol::Key::new(&account, "chat-one");
        let mut draft = Draft { conversation: Some(conversation.clone()), loading: true, ..Draft::default() };
        let visit = draft.visit(&account);
        let failed = || Ok(Response::Error { code: protocol::ErrorCode::Refused, detail: "Update refused".into() });
        assert!(!draft.complete_repair(&visit, Some(&super::tests::account("other")), true, &conversation, failed()));
        assert!(!draft.complete_repair(&visit, Some(&account), false, &conversation, failed()));
        assert!(!draft.complete_repair(&visit, Some(&account), true, &protocol::Key::new(&account, "chat-two"), failed()));
        assert!(draft.loading);
        assert!(draft.error.is_none());
        draft.invalidate_reads();
        assert!(!draft.complete_repair(&visit, Some(&account), true, &conversation, failed()));
        let current = draft.visit(&account);
        assert!(draft.complete_repair(&current, Some(&account), true, &conversation, failed()));
        assert_eq!(draft.error.as_deref(), Some("Update refused"));
    }

    fn removed() -> std::io::Result<Response> {
        Ok(Response::TinyE2ee { status: E2eeStatus {
            revoked: true,
            device: DeviceState::Failed { why: "Removed".into() },
            ..E2eeStatus::default()
        } })
    }

    #[test]
    fn encryption_refresh_device_event_updates_the_open_page_without_resetting_choices() {
        let account = account("one");
        let mut draft = Draft { status: Some(ready()), asked: true, history: "days:7".into(), details: true, backup_mode: Some(true), ..Draft::default() };
        let read = draft.status_changed(&protocol::Change::Tiny { account: account.clone() }, &account, true)
            .expect("a device event must read Status, not just the link");
        assert_eq!(draft.complete_status(&read, Some(&account), true, removed()), Some(false));
        assert!(draft.status.as_ref().unwrap().revoked);
        assert_eq!(draft.history, "days:7");
        assert!(draft.details);
        assert_eq!(draft.backup_mode, Some(true));
    }

    #[test]
    fn encryption_refresh_coalesces_events_and_reads_the_latest_status_after_an_in_flight_reply() {
        let account = account("one");
        let mut draft = Draft::default();
        let first = draft.begin_status(&account, true).unwrap();
        assert!(draft.begin_status(&account, true).is_none(), "only one status request may run at a time");
        assert!(draft.begin_status(&account, true).is_none());
        assert_eq!(draft.complete_status(&first, Some(&account), true, Ok(Response::TinyE2ee { status: ready() })), Some(true));
        let latest = draft.begin_status(&account, true).unwrap();
        assert_eq!(draft.complete_status(&latest, Some(&account), true, removed()), Some(false));
        assert!(draft.status.as_ref().unwrap().revoked);
    }

    #[test]
    fn encryption_refresh_rejects_old_account_overlay_and_page_visit_replies() {
        let one = account("one");
        let two = account("two");
        let mut old = Draft::default();
        let read = old.begin_status(&one, true).unwrap();
        let mut reopened = Draft { status: Some(ready()), ..Draft::default() };
        assert_eq!(reopened.complete_status(&read, Some(&one), true, removed()), None, "closing and reopening creates a new overlay lifetime");
        assert!(!reopened.status.as_ref().unwrap().revoked);
        assert_eq!(old.complete_status(&read, Some(&two), true, removed()), None, "the old account must not write into the current one");
        old.invalidate_reads();
        let current = old.begin_status(&one, true).unwrap();
        assert_eq!(old.complete_status(&read, Some(&one), true, removed()), None, "returning to the tab must supersede the earlier visit");
        assert_eq!(old.complete_status(&current, Some(&one), true, removed()), Some(false));
        assert!(old.status.as_ref().unwrap().revoked);
    }

    #[test]
    fn encryption_refresh_ignores_hidden_pages_and_other_accounts_events() {
        let account = account("one");
        let mut draft = Draft::default();
        assert!(draft.begin_status(&account, false).is_none());
        assert!(draft.status_changed(&protocol::Change::Tiny { account: super::tests::account("other") }, &account, true).is_none());
        assert!(draft.status_changed(&protocol::Change::Status, &account, true).is_none());
        let read = draft.begin_status(&account, true).unwrap();
        assert_eq!(draft.complete_status(&read, Some(&account), false, removed()), None);
        assert!(draft.status.is_none());
    }

    #[test]
    fn encryption_refresh_removal_cancels_link_replies_and_future_progress_reads() {
        let account = account("one");
        let mut draft = Draft { status: Some(ready()), link: Some(protocol::E2eeLink {
            role: "old".into(), stage: protocol::LinkStage::Sending, ..protocol::E2eeLink::default()
        }), ..Draft::default() };
        let status = draft.begin_status(&account, true).unwrap();
        let link = draft.begin_link(&account, true).unwrap();
        assert!(draft.begin_link(&account, true).is_none());
        assert_eq!(draft.complete_status(&status, Some(&account), true, removed()), Some(false));
        assert!(draft.link.is_none());
        assert_eq!(draft.complete_link(&link, Some(&account), true), None);
        assert!(draft.begin_link(&account, true).is_none(), "a removed device must not keep a progress timer alive");
    }

    #[test]
    fn encryption_refresh_background_replies_preserve_an_active_backup_or_action() {
        let account = account("one");
        for answer in [Ok(Response::TinyE2ee { status: ready() }), Err(std::io::Error::from(std::io::ErrorKind::TimedOut))] {
            let mut draft = Draft { status: Some(ready()), loading: true, ..Draft::default() };
            let read = draft.begin_status(&account, true).unwrap();
            assert_eq!(draft.complete_status(&read, Some(&account), true, answer), Some(false));
            assert!(draft.loading, "background status completion must not reenable Save/Import while its action is pending");
        }
    }

    #[test]
    fn encryption_refresh_background_reads_do_not_disable_action_controls() {
        let account = account("one");
        let mut draft = Draft { status: Some(ready()), ..Draft::default() };
        let read = draft.begin_status(&account, true).unwrap();
        assert!(!draft.loading, "only an explicit action owns the backup busy state");
        assert_eq!(draft.complete_status(&read, Some(&account), true, Ok(Response::TinyE2ee { status: ready() })), Some(false));
        assert!(!draft.loading);
    }

    #[test]
    fn the_words_never_claim_more_than_holds() {
        let person = |trust, changed| E2eePerson {
            trust,
            identity_changed: changed,
            ..E2eePerson::default()
        };
        assert_eq!(trust_words(&person(Trust::Verified, false)), "Verified");
        assert!(
            trust_words(&person(Trust::Verified, true)).starts_with("New identity"),
            "a new identity outranks an old verification"
        );
        assert_eq!(
            trust_words(&person(Trust::Changed, false)),
            "Changed since you verified it"
        );
        assert!(state_words(&DeviceState::Off).1.contains("doesn't offer"));
        assert!(
            state_words(&DeviceState::AwaitingApproval { code: "x".into() })
                .1
                .contains("Approve this device")
        );
    }
}
