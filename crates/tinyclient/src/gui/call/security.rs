//! How a Tiny call is protected (`Call::security`): the small lock in the dock's title row, and
//! the panel it opens. The panel says in plain words what the server can and cannot see, shows
//! the call code everyone in the call compares, and each other member's safety number with
//! "Mark as verified".
//!
//! It never says more than holds. "End-to-end encrypted" and the lock appear only while mediad
//! says the group has keys; a call that is only encrypted to the server says so, and why. A key
//! that changed since it was verified, or someone in the encrypted group the server does not
//! list in the call, turns the lock into a warning.

use super::{in_call, Menu};
use crate::gui::components::{glyph, tip};
use crate::gui::state::Shell;
use crate::gui::theme;
use gpui::{div, prelude::*, px, Context, Div, FontWeight, Rgba, Stateful};
use protocol::{Call, CallSecurity, Request, SecureMember, SecurityMode, Trust};

/// What the lock in the dock says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Badge {
    /// Only the people in the call hold the keys.
    EndToEnd,
    /// End-to-end encryption is being set up.
    Pending,
    /// Encrypted between each app and the server only.
    Transport,
    /// End to end, but a verified key changed or the group holds someone the call does not.
    Check,
}

/// Something in the encrypted group is not as it should be: a key changed since it was
/// verified, or a member the server does not list in the call.
pub(super) fn alarming(security: &CallSecurity) -> bool {
    security.mode == SecurityMode::EndToEnd
        && security.members.iter().any(|member| member.trust == Trust::Changed || member.user_id.is_empty())
}

pub(super) fn badge(security: &CallSecurity) -> Badge {
    match security.mode {
        SecurityMode::EndToEnd if alarming(security) => Badge::Check,
        SecurityMode::EndToEnd => Badge::EndToEnd,
        SecurityMode::Pending => Badge::Pending,
        SecurityMode::Transport => Badge::Transport,
    }
}

pub(super) fn badge_words(badge: Badge) -> &'static str {
    match badge {
        Badge::EndToEnd => "End-to-end encrypted",
        Badge::Pending => "Setting up encryption…",
        Badge::Transport => "Encrypted to the server",
        Badge::Check => "Check security",
    }
}

fn badge_look(badge: Badge) -> (&'static str, Rgba) {
    match badge {
        Badge::EndToEnd => ("icons/lock.svg", theme::positive()),
        Badge::Pending => ("icons/clock.svg", theme::text_faint()),
        // Not a lock: the server could hear it. Not alarming either: it is what most calls are.
        Badge::Transport => ("icons/shield.svg", theme::text_faint()),
        Badge::Check => ("icons/triangle-alert.svg", theme::warning()),
    }
}

/// Digits in groups of `size`, a space between: `12345 67890`.
pub(super) fn grouped(digits: &str, size: usize) -> String {
    let chars: Vec<char> = digits.chars().filter(|c| !c.is_whitespace()).collect();
    chars.chunks(size.max(1)).map(|chunk| chunk.iter().collect::<String>()).collect::<Vec<_>>().join(" ")
}

/// `grouped` in lines of `per_line` groups, for a narrow panel.
pub(super) fn grouped_lines(digits: &str, size: usize, per_line: usize) -> Vec<String> {
    let groups: Vec<String> = grouped(digits, size).split(' ').filter(|g| !g.is_empty()).map(str::to_owned).collect();
    groups.chunks(per_line.max(1)).map(|line| line.join(" ")).collect()
}

/// A member's name as the call's roster has it. Someone the server does not list in the call
/// is said to be exactly that.
pub(super) fn member_name(call: &Call, user_id: &str) -> String {
    if user_id.is_empty() {
        return "Someone the server doesn't list".to_owned();
    }
    call.participants
        .iter()
        .find(|who| who.id == user_id)
        .map(|who| who.name.clone())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "Someone".to_owned())
}

fn first_name(name: &str) -> &str {
    name.split_whitespace().next().unwrap_or(name)
}

/// The headline and the sentence under it.
pub(super) fn summary(security: &CallSecurity) -> (&'static str, &'static str) {
    match badge(security) {
        Badge::EndToEnd => ("End-to-end encrypted", "Only the people in this call can hear it. The server passes along audio it cannot unlock."),
        Badge::Check => ("Check who is in this call", "The call is end-to-end encrypted, but something about who holds its keys changed. Read below before you trust it."),
        Badge::Pending => ("Setting up end-to-end encryption", "Your microphone waits until it's done, so nothing you say reaches the server unlocked."),
        Badge::Transport => ("Encrypted to the server", "Encrypted between each app and the server. The server could hear this call."),
    }
}

/// Why a call is only encrypted to the server, one line each.
pub(super) fn why_transport(call: &Call, security: &CallSecurity) -> Vec<String> {
    if security.mode != SecurityMode::Transport {
        return Vec::new();
    }
    if security.server_lacks {
        return vec!["This server doesn't support end-to-end encrypted calls.".to_owned()];
    }
    if security.unsupported.is_empty() {
        return vec!["Someone in this call uses an app that can't encrypt end to end.".to_owned()];
    }
    security
        .unsupported
        .iter()
        .map(|user| format!("{}'s app can't encrypt end to end.", first_name(&member_name(call, user))))
        .collect()
}

/// What a member's row says about their key.
pub(super) fn trust_words(member: &SecureMember) -> &'static str {
    if member.user_id.is_empty() {
        return "Not in the call's list. Ask everyone to compare the call code.";
    }
    match member.trust {
        Trust::Verified => "Verified",
        Trust::Unverified => "Not verified",
        Trust::Changed => "Key changed",
    }
}

/// The loud line under a member whose verified key changed.
pub(super) fn changed_warning(call: &Call, member: &SecureMember) -> Option<String> {
    (member.trust == Trust::Changed).then(|| {
        let name = member_name(call, &member.user_id);
        format!("{name}'s key changed since you verified it. Verify again before trusting this call.")
    })
}

/// What the server can still see while it cannot hear.
const SERVER_SEES: [&str; 4] = [
    "Who is in the call, and when",
    "Who is speaking and how loud, for the speaking ring",
    "Roughly how much audio each person sends",
    "Network addresses",
];

fn label(text: &str) -> Div {
    div()
        .text_size(px(11.))
        .line_height(px(15.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(theme::text_faint())
        .child(text.to_owned())
}

fn words(text: String, color: Rgba) -> Div {
    div().text_size(px(11.5)).line_height(px(16.)).text_color(color).child(text)
}

fn digits(lines: Vec<String>) -> Div {
    div()
        .flex()
        .flex_col()
        .font_family(theme::MONO)
        .text_size(px(13.))
        .line_height(px(18.))
        .text_color(theme::text())
        .children(lines.into_iter().map(|line| div().child(line)))
}

impl Shell {
    /// The lock in the dock's title row, for a call that says how it is protected.
    pub(super) fn call_security_badge(&self, call: &Call, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        let security = call.security.as_ref()?;
        let shown = badge(security);
        let (icon, color) = badge_look(shown);
        Some(
            div()
                .id("call-security")
                .flex_none()
                .flex()
                .items_center()
                .cursor_pointer()
                .child(glyph(icon, 11., color))
                .tooltip(tip(badge_words(shown), None))
                .on_click(cx.listener(|this, _, _, cx| {
                    // The title row opens the conversation; this opens the panel alone.
                    cx.stop_propagation();
                    this.call_menu_toggle(Menu::Security, cx);
                })),
        )
    }

    /// Remembers `member`'s key as verified on the call's account, or forgets it.
    fn call_verify(&mut self, call: &Call, member: &SecureMember, verified: bool, cx: &mut Context<Self>) {
        let request = Request::TinyVerify {
            account: call.conversation.account(),
            user_id: member.user_id.clone(),
            key: member.key.clone(),
            verified,
        };
        self.call_ask(request, cx);
    }

    /// The panel the lock opens.
    pub(super) fn call_security_panel(&self, call: &Call, cx: &mut Context<Self>) -> Stateful<Div> {
        let mut panel = div()
            .id("call-security-panel")
            .max_h(px(440.))
            .overflow_y_scroll()
            .p(px(12.))
            .flex()
            .flex_col()
            .gap(px(10.));
        let Some(security) = call.security.clone() else {
            return panel;
        };
        let shown = badge(&security);
        let (icon, color) = badge_look(shown);
        let (headline, sentence) = summary(&security);
        panel = panel
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(glyph(icon, 16., color))
                    .child(
                        div()
                            .text_size(px(13.))
                            .line_height(px(18.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::text())
                            .child(headline),
                    ),
            )
            .child(words(sentence.to_owned(), theme::text_soft()))
            .children(why_transport(call, &security).into_iter().map(|line| words(line, theme::text_soft())));
        if security.mode != SecurityMode::EndToEnd {
            return panel;
        }
        panel = panel.child(
            div()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(label("THE SERVER CAN STILL SEE"))
                .children(SERVER_SEES.iter().map(|seen| words(format!("· {seen}"), theme::text_soft())))
                .child(words("It cannot hear what is said.".to_owned(), theme::text_soft())),
        );
        if !security.code.is_empty() {
            panel = panel.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(label("CALL CODE"))
                    .child(digits(grouped_lines(&security.code, 5, 3)))
                    .child(words(
                        "Everyone in the call sees the same code. Compare it out loud to be sure no one is listening in. It changes whenever someone joins or leaves."
                            .to_owned(),
                        theme::text_faint(),
                    )),
            );
        }
        if security.members.is_empty() {
            return panel;
        }
        let in_it = in_call(call);
        let mut people = div().flex().flex_col().gap(px(8.)).child(label("SAFETY NUMBERS"));
        for (ix, member) in security.members.iter().enumerate() {
            let name = member_name(call, &member.user_id);
            let ghost = member.user_id.is_empty();
            let loud = ghost || member.trust == Trust::Changed;
            let state_color = match member.trust {
                _ if loud => theme::warning(),
                Trust::Verified => theme::positive(),
                _ => theme::text_faint(),
            };
            let verified = member.trust == Trust::Verified;
            let button = (!ghost && in_it).then(|| {
                let (call, member) = (call.clone(), member.clone());
                div()
                    .id(("call-verify", ix))
                    .flex_none()
                    .h(px(22.))
                    .px(px(8.))
                    .rounded(px(6.))
                    .bg(theme::hover())
                    .flex()
                    .items_center()
                    .text_size(px(11.5))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme::text())
                    .cursor_pointer()
                    .hover(|style| style.bg(theme::selected()))
                    .child(if verified { "Unmark" } else { "Mark as verified" })
                    .on_click(cx.listener(move |this, _, _, cx| this.call_verify(&call, &member, !verified, cx)))
            });
            let mut row = div()
                .flex()
                .flex_col()
                .gap(px(3.))
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
                                .text_size(px(12.5))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(if ghost { theme::warning() } else { theme::text() })
                                .child(name),
                        )
                        .when(verified && !loud, |row| row.child(glyph("icons/badge-check.svg", 13., theme::positive())))
                        .children(button),
                )
                .child(words(trust_words(member).to_owned(), state_color));
            if let Some(warning) = changed_warning(call, member) {
                row = row.child(words(warning, theme::warning()));
            }
            if !member.safety.is_empty() {
                row = row.child(digits(grouped_lines(&member.safety, 5, 3)));
            }
            people = people.child(row);
        }
        panel.child(people).child(words(
            "Compare a safety number with that person, in person or on another channel you trust. Both of you see the same number."
                .to_owned(),
            theme::text_faint(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::{CallState, Key, Network, Participant};

    fn call(security: CallSecurity) -> Call {
        Call {
            conversation: Key { network: Network::Tiny, account: "me".into(), id: "room".into() },
            state: CallState::Joined,
            participants: vec![
                Participant { id: "bea".into(), name: "Bea Lane".into(), ..Participant::default() },
                Participant { id: "cy".into(), name: "Cy".into(), ..Participant::default() },
            ],
            muted: false,
            deafened: false,
            joined: 0,
            input: String::new(),
            output: String::new(),
            engine_linked: true,
            video: Vec::new(),
            speaking: false,
            level: None,
            inputs: Vec::new(),
            outputs: Vec::new(),
            security: Some(security),
        }
    }

    fn member(user: &str, trust: Trust) -> SecureMember {
        SecureMember { user_id: user.into(), key: "aa".into(), safety: "1".repeat(45), trust }
    }

    fn secure(members: Vec<SecureMember>) -> CallSecurity {
        CallSecurity { mode: SecurityMode::EndToEnd, code: "123456789012345678901234567890".into(), epoch: 2, members, ..CallSecurity::default() }
    }

    #[test]
    fn the_badge_says_end_to_end_only_when_it_is() {
        assert_eq!(badge_words(badge(&secure(vec![member("bea", Trust::Unverified)]))), "End-to-end encrypted");
        let pending = CallSecurity { mode: SecurityMode::Pending, ..CallSecurity::default() };
        assert_eq!(badge_words(badge(&pending)), "Setting up encryption…");
        let transport = CallSecurity { mode: SecurityMode::Transport, ..CallSecurity::default() };
        assert_eq!(badge_words(badge(&transport)), "Encrypted to the server");
        // Neither the lock nor its words for anything but end to end.
        for security in [&pending, &transport] {
            assert_ne!(badge_look(badge(security)).0, "icons/lock.svg");
            assert!(!summary(security).0.contains("End-to-end encrypted"));
        }
    }

    #[test]
    fn a_changed_key_or_an_unlisted_member_is_a_warning() {
        assert!(!alarming(&secure(vec![member("bea", Trust::Verified), member("cy", Trust::Unverified)])));
        assert_eq!(badge(&secure(vec![member("bea", Trust::Changed)])), Badge::Check);
        assert_eq!(badge(&secure(vec![member("", Trust::Unverified)])), Badge::Check);
        assert_eq!(badge_words(Badge::Check), "Check security");
        // A transport-only call has no group to be wrong about.
        let transport = CallSecurity { mode: SecurityMode::Transport, members: vec![member("", Trust::Changed)], ..CallSecurity::default() };
        assert_eq!(badge(&transport), Badge::Transport);
    }

    #[test]
    fn codes_are_shown_in_groups_of_five() {
        assert_eq!(grouped("123456789012345678901234567890", 5), "12345 67890 12345 67890 12345 67890");
        assert_eq!(grouped_lines("123456789012345678901234567890", 5, 3), ["12345 67890 12345", "67890 12345 67890"]);
        assert_eq!(grouped_lines(&"9".repeat(45), 5, 3).len(), 3, "a safety number is three lines");
        assert_eq!(grouped("", 5), "");
    }

    #[test]
    fn members_are_named_from_the_roster_and_an_unlisted_one_says_so() {
        let call = call(secure(vec![]));
        assert_eq!(member_name(&call, "bea"), "Bea Lane");
        assert_eq!(member_name(&call, ""), "Someone the server doesn't list");
        assert_eq!(member_name(&call, "zed"), "Someone");
        let changed = member("bea", Trust::Changed);
        assert_eq!(changed_warning(&call, &changed).unwrap(), "Bea Lane's key changed since you verified it. Verify again before trusting this call.");
        assert_eq!(changed_warning(&call, &member("bea", Trust::Verified)), None);
        assert_eq!(trust_words(&member("cy", Trust::Verified)), "Verified");
        assert_eq!(trust_words(&member("cy", Trust::Unverified)), "Not verified");
    }

    #[test]
    fn transport_only_says_why() {
        let lacking = CallSecurity { mode: SecurityMode::Transport, server_lacks: true, ..CallSecurity::default() };
        let call_ = call(lacking.clone());
        assert_eq!(why_transport(&call_, &lacking), ["This server doesn't support end-to-end encrypted calls."]);
        let someone = CallSecurity { mode: SecurityMode::Transport, unsupported: vec!["bea".into()], ..CallSecurity::default() };
        assert_eq!(why_transport(&call_, &someone), ["Bea's app can't encrypt end to end."]);
        assert!(why_transport(&call_, &secure(vec![])).is_empty());
        assert!(summary(&someone).1.contains("The server could hear this call."));
    }
}
