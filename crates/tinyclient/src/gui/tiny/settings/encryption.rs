//! Encryption: this device and the person's other devices, approving a new one, the people this
//! account shares encrypted conversations with and their safety numbers, and the recovery key.
//!
//! It says what holds and no more. A device on the server's list that no device of its owner
//! signed is shown as not approved: the server may have added it, and nothing is encrypted to it.
//! A safety number marked as compared and since changed says so. The recovery key is shown once;
//! key backup is not built yet, and the page says that too.

use super::kit::{self, card};
use super::page_head;
use crate::client::ask;
use crate::gui::components::{button, glyph, ButtonKind};
use crate::gui::state::Shell;
use crate::gui::theme::{self, Type};
use crate::gui::tiny::Page as Facts;
use gpui::{div, prelude::*, px, AnyElement, ClipboardItem, Context, Div, SharedString, Window};
use protocol::{
    grouped_safety, DeviceState, E2eeAsk, E2eeDevice, E2eePerson, E2eeStatus, Request, Response,
    Trust,
};

/// What the page has read and what it is doing.
#[derive(Default)]
pub(in crate::gui) struct Draft {
    pub status: Option<E2eeStatus>,
    pub loading: bool,
    pub asked: bool,
    pub error: Option<String>,
    /// What the last action said, when it went through.
    pub note: Option<String>,
    /// A recovery key just made: shown until the page closes.
    pub recovery: Option<String>,
    /// The person whose devices are open, fetched now.
    pub person: Option<E2eePerson>,
}

/// The words a device's state reads as.
pub(in crate::gui) fn state_words(state: &DeviceState) -> (&'static str, String) {
    match state {
        DeviceState::Off => ("icons/lock.svg", "This server doesn't offer end-to-end encrypted messages.".to_owned()),
        DeviceState::Ready => ("icons/shield.svg", "This device is set up. It can read and send end-to-end encrypted messages from now on.".to_owned()),
        DeviceState::AwaitingApproval { .. } => (
            "icons/key.svg",
            "You already use another device on this server. Approve this one there: open Encryption on that device and paste the link code below.".to_owned(),
        ),
        DeviceState::Failed { why } => ("icons/triangle-alert.svg", why.clone()),
    }
}

/// How a person's safety number stands, in words.
pub(in crate::gui) fn trust_words(person: &E2eePerson) -> &'static str {
    if person.identity_changed {
        return "New identity: none of their devices chain to the ones you knew. Their new devices are not let into your conversations until you accept it.";
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
        account: &protocol::AccountKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.tiny_state.account.as_ref() != Some(account) || self.tiny_state.community.is_none()
        {
            return;
        }
        self.tiny_open_settings(super::Page::Encryption, None, window, cx);
    }

    pub(in crate::gui) fn tiny_e2ee_load(&mut self, cx: &mut Context<Self>) {
        let Some(account) = self.tiny_state.account.clone() else {
            return;
        };
        let Some(draft) = self
            .tiny_state
            .settings
            .as_mut()
            .map(|overlay| &mut overlay.encryption)
        else {
            return;
        };
        draft.loading = true;
        draft.asked = true;
        cx.notify();
        self.off_thread(
            cx,
            move || {
                ask(&Request::TinyE2ee {
                    account,
                    ask: E2eeAsk::Status,
                })
            },
            move |this, answer, cx| {
                let Some(draft) = this
                    .tiny_state
                    .settings
                    .as_mut()
                    .map(|overlay| &mut overlay.encryption)
                else {
                    return;
                };
                draft.loading = false;
                match answer {
                    Ok(Response::TinyE2ee { status }) => {
                        draft.status = Some(status);
                        draft.error = None;
                    }
                    Ok(Response::Error { detail, .. }) => draft.error = Some(detail),
                    Ok(_) => draft.error = Some(crate::client::OTHER_ANSWER.to_owned()),
                    Err(err) => draft.error = Some(crate::client::trouble_words("tiny", &err)),
                }
                cx.notify();
            },
        );
    }

    /// Sends one ask; on `Ok` says `done` and reads the page again.
    fn tiny_e2ee_do(&mut self, ask_for: E2eeAsk, done: &'static str, cx: &mut Context<Self>) {
        let Some(account) = self.tiny_state.account.clone() else {
            return;
        };
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
                let Some(draft) = this
                    .tiny_state
                    .settings
                    .as_mut()
                    .map(|overlay| &mut overlay.encryption)
                else {
                    return;
                };
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

    fn tiny_e2ee_approve(&mut self, cx: &mut Context<Self>) {
        let code = self
            .tiny_state
            .settings_fields
            .link_code
            .read(cx)
            .text()
            .trim()
            .to_owned();
        if code.is_empty() {
            return;
        }
        self.tiny_state
            .settings_fields
            .link_code
            .update(cx, |input, cx| input.clear(cx));
        self.tiny_e2ee_do(
            E2eeAsk::Approve { code },
            "Approved. The new device can read and send from now on.",
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

fn device_row(
    id: impl Into<SharedString>,
    device: &E2eeDevice,
    revoke: Option<gpui::Stateful<Div>>,
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
    kit::table_row(id.into(), false)
        .child(glyph(
            if device.untrusted {
                "icons/triangle-alert.svg"
            } else {
                "icons/key.svg"
            },
            16.,
            tone,
        ))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .truncate()
                        .type_ui()
                        .font_weight(theme::MEDIUM)
                        .text_color(tone)
                        .child(title),
                )
                .child(
                    div()
                        .truncate()
                        .type_small()
                        .font_family(theme::MONO)
                        .text_color(theme::text_soft())
                        .child(device.fingerprint.clone()),
                )
                .when(device.untrusted, |column| {
                    column.child(
                        div()
                            .whitespace_normal()
                            .type_small()
                            .text_color(theme::danger())
                            .child(
                                "Not approved by any of their devices. Nothing is encrypted to it.",
                            ),
                    )
                })
                .when(!since.is_empty(), |column| {
                    column.child(
                        div()
                            .type_small()
                            .text_color(theme::text_faint())
                            .child(format!("Added {since}")),
                    )
                }),
        )
        .children(revoke)
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
    let mut page = div().flex().flex_col().gap(px(20.)).child(page_head(
        "Encryption",
        "End-to-end encrypted conversations can be read only on the members' devices. The server stores ciphertext and can't read them. It still sees who is in a conversation, when messages are sent and how large they are.",
    ));
    if let Some(error) = &draft.error {
        page = page.child(kit::note(
            "icons/triangle-alert.svg",
            theme::danger(),
            error.clone(),
        ));
    }
    if let Some(note) = &draft.note {
        page = page.child(kit::note(
            "icons/shield.svg",
            theme::positive(),
            note.clone(),
        ));
    }
    let Some(status) = &draft.status else {
        let words = if draft.loading {
            "Reading this device's encryption…"
        } else {
            "Nothing read yet."
        };
        return page
            .child(div().type_ui().text_color(theme::text_soft()).child(words))
            .into_any_element();
    };

    // This device.
    let (icon, words) = state_words(&status.device);
    let mut this = card().p(px(12.)).gap(px(10.)).child(
        div()
            .flex()
            .items_start()
            .gap(px(10.))
            .child(glyph(icon, 16., theme::text_soft()))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .whitespace_normal()
                    .type_ui()
                    .text_color(theme::text())
                    .child(words),
            ),
    );
    if !status.fingerprint.is_empty() {
        this = this
            .child(
                div()
                    .type_small()
                    .text_color(theme::text_faint())
                    .child("This device's key fingerprint"),
            )
            .child(
                div()
                    .type_ui()
                    .font_family(theme::MONO)
                    .text_color(theme::text())
                    .child(status.fingerprint.clone()),
            );
    }
    if let DeviceState::AwaitingApproval { code } = &status.device {
        let copied = code.clone();
        this = this
            .child(
                div()
                    .type_small()
                    .text_color(theme::text_faint())
                    .child("Link code"),
            )
            .child(
                div()
                    .whitespace_normal()
                    .type_small()
                    .font_family(theme::MONO)
                    .text_color(theme::text())
                    .child(code.clone()),
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
    page = page.child(super::section("This device").child(this));
    if status.device == DeviceState::Off {
        return page.into_any_element();
    }

    // Your devices.
    let mut mine = card();
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
    let approve = div()
        .p(px(12.))
        .flex()
        .flex_col()
        .gap(px(8.))
        .child(div().whitespace_normal().type_small().text_color(theme::text_soft()).child("Setting up tinyclient on another computer? It shows a link code. Paste it here to approve that device. Check that its fingerprint matches what it shows."))
        .child(div().flex().gap(px(8.)).child(kit::input(&fields.link_code, None, window, cx).flex_1()).child(button("tiny-e2ee-approve", ButtonKind::Primary, "Approve").on_click(cx.listener(|this, _, _, cx| this.tiny_e2ee_approve(cx)))));
    page = page.child(super::section("Your devices").child(mine.child(approve)));

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
        let safety = if person.safety.is_empty() {
            "No safety number yet".to_owned()
        } else {
            grouped_safety(&person.safety)
        };
        people = people.child(
            kit::table_row(SharedString::from(format!("tiny-e2ee-person-{ix}")), false)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .child(
                            div()
                                .truncate()
                                .type_ui()
                                .font_weight(theme::MEDIUM)
                                .text_color(theme::text())
                                .child(name),
                        )
                        .child(
                            div()
                                .whitespace_normal()
                                .type_small()
                                .font_family(theme::MONO)
                                .text_color(theme::text())
                                .child(safety),
                        )
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
                .child("Nobody yet. People appear here once you share an encrypted conversation."),
        );
    }
    page = page.child(
        super::section("Safety numbers")
            .child(div().whitespace_normal().type_small().text_color(theme::text_soft()).child("Compare a safety number with the person in a call or face to face. If it matches on both screens, nobody, the server included, has put a device of their own between you."))
            .child(people),
    );
    if let Some(person) = &draft.person {
        let mut list = card();
        for (ix, device) in person.devices.iter().enumerate() {
            list = list.child(device_row(
                SharedString::from(format!("tiny-e2ee-theirs-{ix}")),
                device,
                None,
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
        "A recovery key is 128 random bits, shown once. Write it down and keep it somewhere safe. Encrypted key backup is not available yet: for now the key backs nothing up, and losing every device still means a new identity.",
    ));
    if let Some(key) = &draft.recovery {
        let copied = key.clone();
        recovery = recovery
            .child(
                div()
                    .type_title()
                    .font_family(theme::MONO)
                    .text_color(theme::text())
                    .child(key.clone()),
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
                .child(kit::input(&fields.recovery_check, None, window, cx).flex_1())
                .child(
                    button("tiny-e2ee-check", ButtonKind::Ghost, "Check")
                        .on_click(cx.listener(|this, _, _, cx| this.tiny_e2ee_check(cx))),
                ),
        );
    }
    page.child(super::section("Recovery key").child(recovery))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

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
                .contains("Approve this one there")
        );
    }
}
