//! The call screen's faces that are not a call that is up: a ring coming in, a call that did
//! not happen, and one the other side ended. The words are the dock's (07 §3, §4, §8); a phone
//! says them on the whole screen, with buttons a thumb can hit.

use super::hooks::{self, Permission};
use super::stage::halo;
use super::{pictures_network, round, Look, Shell};
use crate::gui::call::dock::{failure, Act, Failure};
use crate::gui::call::{Ended, Subject};
use crate::gui::components::{glyph, network_icon};
use crate::gui::phone::tokens::{self, PhoneType};
use crate::gui::theme;
use gpui::{div, prelude::*, px, Context, Div, MouseButton, SharedString, Stateful};
use protocol::{Call, CallFailure, ConversationKind, Key};

/// A full-width button of a failure or an ending: the first is the way forward.
pub(super) fn wide(id: impl Into<SharedString>, words: impl Into<SharedString>, primary: bool) -> Stateful<Div> {
    div()
        .id(gpui::ElementId::Name(id.into()))
        .w_full()
        .h(px(52.))
        .rounded(px(26.))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .phone_row()
        .font_weight(theme::SEMIBOLD)
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .map(|button| {
            if primary {
                button.bg(theme::text()).text_color(theme::canvas()).active(|style| style.bg(theme::text_soft()))
            } else {
                button.bg(theme::selected()).text_color(theme::text()).active(|style| style.bg(theme::pressed()))
            }
        })
        .child(words.into())
}

/// A failure as a phone says it. A refused microphone names this phone's Settings, and offers
/// the way there and a second try once it is allowed.
pub(super) fn phone_failure(call: &Call, subject: &Subject, reason: CallFailure, detail: &str) -> Failure {
    let mut said = failure(call, subject, reason, detail);
    let refused = said.acts.contains(&Act::Settings) || (reason == CallFailure::Audio && detail.to_lowercase().contains("system settings"));
    if refused {
        said.title = "tinyclient can't use the microphone".to_owned();
        said.line = "Allow microphone access for tinyclient in Settings, then call again.".to_owned();
        said.icon = "icons/mic-off.svg";
        said.acts = vec![Act::Settings, Act::Again("Try again")];
    }
    said
}

impl Shell {
    /// Someone is calling: who, large; Decline and Accept at the bottom, and Accept with video
    /// where the call can carry pictures. Nothing here answers by itself.
    pub(super) fn call_phone_incoming(&self, call: &Call, subject: &Subject, from: &str, cx: &mut Context<Self>) -> Div {
        let network = call.conversation.network;
        let direct = subject.kind == ConversationKind::Direct;
        let face = self.call_phone_face(call, subject, call.participants.first().map_or("", |peer| peer.id.as_str()));
        let lead = halo(self.call_phone_lead(&call.conversation, subject, face, 144., false), 144., false);
        let kind = if pictures_network(call) { "Incoming call" } else { "Incoming voice call" };
        let line = if direct { kind.to_owned() } else { format!("{from} is calling") };
        let decline = round("call-phone-decline", "icons/phone-off.svg", "Decline", Look::End, 72.).on_click(cx.listener(|this, _, _, cx| this.call_decline(cx)));
        let video = pictures_network(call).then(|| {
            round("call-phone-accept-video", "icons/video.svg", "Video", Look::Accept, 72.).on_click(cx.listener(|this, _, _, cx| {
                this.call_state.phone.video_on_join = true;
                this.call_accept(cx);
            }))
        });
        let accept = round("call-phone-accept", "icons/phone.svg", "Accept", Look::Accept, 72.).on_click(cx.listener(|this, _, _, cx| this.call_accept(cx)));
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .bg(theme::over(gpui::Rgba { alpha: 0.10, ..theme::positive() }, theme::canvas()))
            .child(div().flex_none().h(px(tokens::safe_top() + 72.)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .mb(px(36.))
                    .phone_preview()
                    .text_color(theme::text_soft())
                    .child(network_icon(network, 14.))
                    .child(subject.space.clone().unwrap_or_else(|| network.label().to_owned())),
            )
            .child(lead)
            .child(
                div()
                    .mt(px(28.))
                    .px(px(24.))
                    .max_w_full()
                    .text_center()
                    .whitespace_normal()
                    .line_clamp(2)
                    .text_size(px(tokens::scaled(30.)))
                    .line_height(px(tokens::scaled(36.)))
                    .font_weight(theme::SEMIBOLD)
                    .child(subject.title.clone()),
            )
            .child(div().mt(px(8.)).px(px(24.)).phone_row().text_color(theme::text_soft()).child(line))
            .child(div().flex_1())
            .child(
                div()
                    .w_full()
                    .px(px(36.))
                    .pb(px(tokens::safe_bottom() + 36.))
                    .flex()
                    .justify_between()
                    .child(decline.flex_none().w(px(88.)))
                    .children(video.map(|video| video.flex_none().w(px(88.))))
                    .child(accept.flex_none().w(px(88.))),
            )
    }

    /// A call that did not happen: what went wrong in words, then what can be done about it.
    /// It stays until it is closed or the next call starts.
    pub(super) fn call_phone_failed(&self, call: &Call, subject: &Subject, reason: CallFailure, detail: &str, cx: &mut Context<Self>) -> Div {
        let Failure { title, line, icon, acts } = phone_failure(call, subject, reason, detail);
        let line = self.call_state.error.clone().unwrap_or(line);
        let network = call.conversation.network;
        let buttons = acts.into_iter().enumerate().map(|(ix, act)| {
            let key: Key = call.conversation.clone();
            let name = subject.title.clone();
            let primary = ix == 0;
            match act {
                Act::Again(word) => wide("call-phone-again", word, primary).on_click(cx.listener(move |this, _, _, cx| this.call_start(key.clone(), cx))),
                Act::Message => wide("call-phone-message", "Message", primary).on_click(cx.listener(move |this, _, _, cx| {
                    this.call_leave(cx);
                    this.call_go(key.clone(), &name, cx);
                })),
                Act::Handoff => wide("call-phone-handoff", crate::gui::call::handoff_label(network), primary)
                    .on_click(cx.listener(move |this, _, _, cx| this.call_handoff(key.clone(), cx))),
                Act::Settings => wide("call-phone-settings", "Open Settings", primary).on_click(cx.listener(|_, _, _, _| hooks::open_system_settings(Permission::Microphone))),
            }
        });
        let close = wide("call-phone-close", "Close", false).bg(gpui::transparent_black()).text_color(theme::text_soft()).on_click(cx.listener(|this, _, _, cx| this.call_leave(cx)));
        self.call_phone_message_screen(
            div().size(px(84.)).rounded_full().bg(theme::warning_soft()).flex().items_center().justify_center().child(glyph(icon, 34., theme::warning())),
            title,
            line,
            buttons.collect(),
            close,
        )
    }

    /// The other side ended the call: with whom, and for how long. It goes by itself.
    pub(super) fn call_phone_over(&self, ended: &Ended, cx: &mut Context<Self>) -> Div {
        let lead = self.call_phone_lead(&ended.conversation, &ended.subject, None, 96., false);
        let again = (ended.subject.kind == ConversationKind::Direct).then(|| {
            let key = ended.conversation.clone();
            wide("call-phone-call-again", "Call again", true).on_click(cx.listener(move |this, _, _, cx| this.call_start(key.clone(), cx)))
        });
        let close = wide("call-phone-close", "Close", again.is_none()).on_click(cx.listener(|this, _, _, cx| this.call_leave(cx)));
        self.call_phone_message_screen(lead, ended.subject.title.clone(), format!("Call ended · {}", super::clock(ended.seconds)), again.into_iter().collect(), close)
    }

    /// A screen that says one thing: a picture, a title and a line in the middle, buttons at
    /// the bottom.
    fn call_phone_message_screen(&self, lead: Div, title: String, line: String, buttons: Vec<Stateful<Div>>, close: Stateful<Div>) -> Div {
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .px(px(24.))
            .child(div().flex_none().h(px(tokens::safe_top())))
            .child(
                div()
                    .flex_1()
                    .w_full()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(10.))
                    .child(div().mb(px(14.)).child(lead))
                    .child(
                        div()
                            .max_w(px(320.))
                            .text_center()
                            .whitespace_normal()
                            .text_size(px(tokens::scaled(22.)))
                            .line_height(px(tokens::scaled(28.)))
                            .font_weight(theme::SEMIBOLD)
                            .child(title),
                    )
                    .child(div().max_w(px(300.)).text_center().whitespace_normal().phone_row().text_color(theme::text_soft()).child(line)),
            )
            .child(div().w_full().max_w(px(380.)).flex().flex_col().gap(px(10.)).pb(px(tokens::safe_bottom() + 16.)).children(buttons).child(close))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::{CallState, Network};

    fn call() -> Call {
        Call {
            conversation: Key { network: Network::Discord, account: "1".into(), id: "9".into() },
            state: CallState::Connecting,
            participants: Vec::new(),
            muted: false,
            deafened: false,
            joined: 0,
            input: String::new(),
            output: String::new(),
            engine_linked: true,
            video: Vec::new(),
            speaking: false,
            level: None,
            output_level: None,
            input_id: String::new(),
            output_id: String::new(),
            device_notice: None,
            device_stuck: false,
            inputs: Vec::new(),
            outputs: Vec::new(),
            security: None,
            picture: None,
        }
    }

    #[test]
    fn a_refused_microphone_names_the_phones_settings_and_offers_a_second_try() {
        let mara = Subject { title: "Mara Okafor".into(), kind: ConversationKind::Direct, avatar: None, space: None };
        for detail in [
            "Allow it in System Settings › Privacy & Security › Microphone, then call again.",
            "Allow the microphone in the system settings, then call again.",
        ] {
            let said = phone_failure(&call(), &mara, CallFailure::Audio, detail);
            assert_eq!(said.title, "tinyclient can't use the microphone");
            assert_eq!(said.acts, [Act::Settings, Act::Again("Try again")]);
        }
        let other = phone_failure(&call(), &mara, CallFailure::NoAnswer, "");
        assert_eq!(other.title, "No answer", "the rest read as the dock reads them");
    }
}
