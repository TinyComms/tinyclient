//! First-run setup, drawn: the screen an owner is shown the first time they open a server nobody
//! has set up, and the form that screen shares with Server Settings › Privacy. What is shown and
//! sent is decided in `crate::tiny_setup`; the words are the server's, read from `GET /v1/setup`.
//!
//! Nothing here is kept. The server says whether setup is needed, and what it keeps now; what
//! this window holds is the form the owner is filling in, and "Later" only lasts until the server
//! is opened again. Nothing is applied until the owner says so, and nothing blocks the app.

use super::settings::{check_box, section, Page as SettingsPage};
use super::Page;
use crate::client::ask;
use crate::gui::components::{button, button_disabled, glyph, ButtonKind};
use crate::gui::state::Shell;
use crate::gui::theme::{self, Type};
use crate::tiny_setup::{self, Form, Hint, Remedy};
use gpui::{div, prelude::*, px, AnyElement, Context, Div, ElementId, SharedString, Stateful};
use protocol::{Request, Response, TinySetup, TinySetupApply, TinySetupRefusal};

/// What the window holds of setup, for the server on show.
#[derive(Default)]
pub(super) struct State {
    /// `GET /v1/setup` as last read. `None` until read, and always for anyone but an owner.
    pub(super) data: Option<TinySetup>,
    /// A read is out.
    pub(super) reading: bool,
    /// Why the last read failed, in words, for the page that wanted it.
    pub(super) read_error: Option<String>,
    read_epoch: u64,
    /// The form the owner is filling in.
    pub(super) form: Form,
    /// The first-run screen is up.
    pub(super) prompt: bool,
    /// "Later" was chosen: not asked again until this server is opened again.
    put_off: bool,
    /// The choices are open under the presets.
    customizing: bool,
    /// The Privacy page shows the form, not only what is chosen.
    pub(super) changing: bool,
    /// An answer is with the server.
    pub(super) sending: bool,
    send_epoch: u64,
    /// The server would not apply it, and said what to do.
    refusal: Option<TinySetupRefusal>,
    /// Anything else it said no to, in words.
    error: Option<String>,
    /// A passkey link is being asked for.
    linking: bool,
    /// What happened with the link, in words.
    link_note: Option<String>,
}

/// What an answer is made of when it is sent.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Answer {
    /// What the form holds.
    Form,
    /// "Keep what I have": nothing changes and setup is done.
    Keep,
}

impl State {
    /// The form back to what the server keeps now.
    pub(super) fn reset_form(&mut self) {
        self.form = self.data.as_ref().map(Form::starting).unwrap_or_default();
        self.refusal = None;
        self.error = None;
        self.link_note = None;
    }

    /// The server is being opened: what was put off with "Later" is asked again.
    pub(super) fn put_off_ends(&mut self) {
        self.put_off = false;
    }

    /// The owner changed the form: whatever the server said to the last answer is out of date.
    fn touched(&mut self) {
        self.refusal = None;
        self.error = None;
        self.link_note = None;
    }

    /// Server Settings has something to save.
    pub(super) fn dirty(&self) -> bool {
        self.data.as_ref().is_some_and(|setup| self.form.unsaved(setup))
    }
}

/// A selectable card: lit when chosen, faint and quiet when it cannot be.
fn pick_card(id: impl Into<ElementId>, on: bool, enabled: bool) -> Stateful<Div> {
    div()
        .id(id)
        .w_full()
        .p(px(14.))
        .rounded(px(theme::R_CARD))
        .bg(if on { theme::accent_wash() } else { theme::raised() })
        .border_1()
        .border_color(if on { theme::accent() } else { theme::hairline() })
        .flex()
        .items_start()
        .gap(px(12.))
        .when(enabled, |card| card.cursor_pointer().when(!on, |card| card.hover(|style| style.border_color(theme::border_strong()))))
        .when(!enabled, |card| card.opacity(0.5))
}

/// A round mark, filled when it is the one picked.
fn radio(on: bool) -> Div {
    let ring = div().flex_none().mt(px(2.)).size(px(14.)).rounded(px(7.)).flex().items_center().justify_center();
    if on {
        ring.bg(theme::accent()).child(div().size(px(6.)).rounded(px(3.)).bg(theme::on_accent()))
    } else {
        ring.border_1().border_color(theme::text_faint())
    }
}

fn title(words: impl Into<SharedString>) -> Div {
    div().type_ui().font_weight(theme::MEDIUM).text_color(theme::text()).child(words.into())
}

fn about(words: impl Into<SharedString>) -> Div {
    div().whitespace_normal().type_small().text_color(theme::text_soft()).child(words.into())
}

/// A quiet card of words: a glyph, then words that wrap.
fn aside(icon: &'static str, tone: gpui::Rgba, words: impl Into<SharedString>) -> Div {
    div()
        .w_full()
        .p(px(12.))
        .rounded(px(theme::R_CARD))
        .bg(theme::raised())
        .border_1()
        .border_color(theme::hairline())
        .flex()
        .items_start()
        .gap(px(10.))
        .child(div().pt(px(2.)).child(glyph(icon, 16., tone)))
        .child(div().flex_1().min_w_0().whitespace_normal().type_ui().text_color(theme::text_soft()).child(words.into()))
}

impl Shell {
    // What arrives.

    /// A server page has just been read. The first read of a server resets what was held of
    /// another; an owner's is asked for the outline and, if setup is still needed, shown it. Later
    /// reads (the live socket said something changed) only refresh it when the server now says
    /// something else, which also puts the first-run screen away if another device finished it.
    pub(super) fn tiny_setup_page_loaded(&mut self, first: bool, cx: &mut Context<Self>) {
        if first {
            self.tiny_state.setup = State::default();
        }
        let Some(community) = self.tiny_state.community.as_ref() else {
            return;
        };
        let held = &self.tiny_state.setup;
        let now = tiny_setup::Held { data: held.data.as_ref(), busy: held.reading || held.sending, prompt: held.prompt, put_off: held.put_off };
        match tiny_setup::after_page_read(community, &now) {
            tiny_setup::Next::Nothing => {}
            // Not an owner, or not on a server with setup (any more): none of it is kept.
            tiny_setup::Next::Forget => self.tiny_state.setup = State::default(),
            tiny_setup::Next::Read { prompt } => self.tiny_setup_read(prompt, cx),
            tiny_setup::Next::Prompt => {
                let held = &mut self.tiny_state.setup;
                held.prompt = true;
                held.customizing = false;
                cx.notify();
            }
        }
    }
    /// Reads the outline. With `prompt`, the first-run screen opens if the server says setup is
    /// needed.
    pub(super) fn tiny_setup_read(&mut self, prompt: bool, cx: &mut Context<Self>) {
        let Some(account) = self.tiny_state.account.clone() else {
            return;
        };
        if !self.tiny_state.community.as_ref().is_some_and(tiny_setup::may_choose) {
            return;
        }
        let state = &mut self.tiny_state.setup;
        state.reading = true;
        state.read_error = None;
        state.read_epoch += 1;
        let epoch = state.read_epoch;
        let wanted = account.clone();
        self.off_thread(cx, move || ask(&Request::TinySetupRead { account }), move |this, answer, cx| {
            if this.tiny_state.setup.read_epoch != epoch || this.tiny_state.account.as_ref() != Some(&wanted) {
                return;
            }
            this.tiny_state.setup.reading = false;
            match answer {
                Ok(Response::TinySetup { setup }) => this.tiny_setup_loaded(setup, prompt),
                Ok(Response::Error { detail, .. }) => this.tiny_state.setup.read_error = Some(detail),
                Ok(_) => this.tiny_state.setup.read_error = Some(crate::client::OTHER_ANSWER.to_owned()),
                Err(err) => this.tiny_state.setup.read_error = Some(crate::client::trouble_words("tiny", &err)),
            }
            cx.notify();
        });
    }

    fn tiny_setup_loaded(&mut self, setup: TinySetup, prompt: bool) {
        let ask_now = prompt && self.tiny_state.community.as_ref().is_some_and(|community| tiny_setup::asks_now(community, &setup, self.tiny_state.setup.put_off));
        let state = &mut self.tiny_state.setup;
        // What the owner has already picked stays; a form they have not touched follows the server.
        let touched = state.data.as_ref().is_some_and(|data| state.form.answers(data));
        if !touched {
            state.form = Form::starting(&setup);
        }
        // Another device answered while this screen was up.
        if !setup.needed {
            state.prompt = false;
        }
        if ask_now {
            state.prompt = true;
            state.customizing = false;
        }
        state.data = Some(setup);
    }

    // What the owner does.

    fn tiny_setup_pick_preset(&mut self, id: &str, cx: &mut Context<Self>) {
        let state = &mut self.tiny_state.setup;
        let Some(setup) = state.data.as_ref() else {
            return;
        };
        if state.form.pick_preset(setup, id) {
            state.touched();
        }
        cx.notify();
    }

    fn tiny_setup_pick_option(&mut self, choice: &str, option: &str, cx: &mut Context<Self>) {
        let state = &mut self.tiny_state.setup;
        let Some(setup) = state.data.as_ref() else {
            return;
        };
        if state.form.pick_option(setup, choice, option) {
            state.touched();
        }
        cx.notify();
    }

    fn tiny_setup_toggle_erase(&mut self, cx: &mut Context<Self>) {
        let state = &mut self.tiny_state.setup;
        state.form.erase_emails = !state.form.erase_emails;
        state.touched();
        cx.notify();
    }

    /// "Change" on the Privacy page.
    pub(super) fn tiny_privacy_change(&mut self, cx: &mut Context<Self>) {
        self.tiny_state.setup.changing = true;
        cx.notify();
    }

    fn tiny_setup_toggle_customize(&mut self, cx: &mut Context<Self>) {
        let state = &mut self.tiny_state.setup;
        state.customizing = !state.customizing;
        cx.notify();
    }

    /// "Later": the first-run screen goes, and comes again the next time this server is opened.
    pub(super) fn tiny_setup_later(&mut self, cx: &mut Context<Self>) {
        let state = &mut self.tiny_state.setup;
        state.prompt = false;
        state.put_off = true;
        cx.notify();
    }

    /// Sends the form, or "Keep what I have". Nothing is applied until this.
    fn tiny_setup_send(&mut self, what: Answer, cx: &mut Context<Self>) {
        let Some(account) = self.tiny_state.account.clone() else {
            return;
        };
        let state = &mut self.tiny_state.setup;
        let Some(setup) = state.data.as_ref() else {
            return;
        };
        if state.sending {
            return;
        }
        let apply: TinySetupApply = match what {
            Answer::Keep => tiny_setup::keep(),
            Answer::Form => state.form.apply(setup),
        };
        state.sending = true;
        state.touched();
        state.send_epoch += 1;
        let epoch = state.send_epoch;
        let wanted = account.clone();
        if let Some(overlay) = self.tiny_state.settings.as_mut().filter(|overlay| overlay.page == SettingsPage::Privacy) {
            overlay.saving = true;
            overlay.error = None;
        }
        cx.notify();
        self.off_thread(cx, move || ask(&Request::TinySetupApply { account, apply }), move |this, answer, cx| {
            if this.tiny_state.setup.send_epoch != epoch || this.tiny_state.account.as_ref() != Some(&wanted) {
                return;
            }
            this.tiny_setup_answered(wanted, answer, cx);
        });
    }

    fn tiny_setup_answered(&mut self, account: protocol::AccountKey, answer: std::io::Result<Response>, cx: &mut Context<Self>) {
        self.tiny_state.setup.sending = false;
        match answer {
            Ok(Response::TinySetupApplied { applied }) => {
                // The server is the source of truth: what it holds is read again, and this window
                // keeps nothing of the answer but the words it shows once.
                let state = &mut self.tiny_state.setup;
                state.prompt = false;
                state.put_off = false;
                state.changing = false;
                state.reset_form();
                self.toast(Some(("icons/check.svg", theme::text_soft())), tiny_setup::applied_words(&applied), cx);
                self.tiny_settings_saved(cx);
                self.fetch(account, None, cx);
                self.tiny_setup_read(false, cx);
            }
            Ok(Response::TinySetupRefused { refusal }) => {
                self.tiny_state.setup.refusal = Some(refusal);
                self.tiny_settings_unsaved();
            }
            Ok(Response::Error { detail, .. }) => self.tiny_setup_failed(detail),
            Ok(_) => self.tiny_setup_failed(crate::client::OTHER_ANSWER.to_owned()),
            Err(err) => self.tiny_setup_failed(crate::client::trouble_words("tiny", &err)),
        }
        cx.notify();
    }

    /// Said under the form; on Server Settings it is also in the bar that holds Save.
    fn tiny_setup_failed(&mut self, detail: String) {
        self.tiny_settings_failed(&detail);
        self.tiny_state.setup.error = Some(detail);
    }

    fn tiny_setup_remedy(&mut self, remedy: Remedy, cx: &mut Context<Self>) {
        match remedy {
            Remedy::AddPasskey => self.tiny_setup_link(cx),
            Remedy::DoItAnyway => {
                self.tiny_state.setup.form.allow_lockout = true;
                self.tiny_setup_send(Answer::Form, cx);
            }
        }
    }

    /// Asks the server for a link that adds a passkey for this account. chatd opens it in the
    /// system browser; here the owner is told it did.
    fn tiny_setup_link(&mut self, cx: &mut Context<Self>) {
        let Some(account) = self.tiny_state.account.clone() else {
            return;
        };
        let state = &mut self.tiny_state.setup;
        if state.linking {
            return;
        }
        state.linking = true;
        state.link_note = None;
        cx.notify();
        self.off_thread(cx, move || ask(&Request::TinyPasskeyLink { account }), |this, answer, cx| {
            let state = &mut this.tiny_state.setup;
            state.linking = false;
            state.link_note = Some(match answer {
                Ok(Response::Ok) => "A page to add a passkey opened in your browser. When you have added one, come back and check again.".to_owned(),
                Ok(Response::Error { detail, .. }) => detail,
                Ok(_) => crate::client::OTHER_ANSWER.to_owned(),
                Err(err) => crate::client::trouble_words("tiny", &err),
            });
            cx.notify();
        });
    }

    /// The owner has added a passkey (or says so): what the server counts is read again.
    fn tiny_setup_check_again(&mut self, cx: &mut Context<Self>) {
        let state = &mut self.tiny_state.setup;
        state.link_note = None;
        state.refusal = None;
        self.tiny_setup_read(false, cx);
        cx.notify();
    }

    // What Server Settings › Privacy asks of it.

    /// The page comes on show: an owner's is the server's outline, read if it is not held.
    pub(super) fn tiny_privacy_enter(&mut self, cx: &mut Context<Self>) {
        let held = &self.tiny_state.setup;
        if held.data.is_none() && !held.reading && self.tiny_state.community.as_ref().is_some_and(tiny_setup::may_choose) {
            self.tiny_setup_read(false, cx);
        }
    }

    pub(super) fn tiny_privacy_reset(&mut self) {
        self.tiny_state.setup.reset_form();
    }

    pub(super) fn tiny_privacy_dirty(&self) -> bool {
        self.tiny_state.community.as_ref().is_some_and(tiny_setup::may_choose) && self.tiny_state.setup.dirty()
    }

    pub(super) fn tiny_privacy_save(&mut self, cx: &mut Context<Self>) {
        self.tiny_setup_send(Answer::Form, cx);
    }

    /// "Keep what I have", from the page: an empty answer, and setup is done.
    pub(super) fn tiny_privacy_keep(&mut self, cx: &mut Context<Self>) {
        self.tiny_setup_send(Answer::Keep, cx);
    }

    /// Reads the outline again, from "Try again".
    pub(super) fn tiny_privacy_reread(&mut self, cx: &mut Context<Self>) {
        self.tiny_setup_read(false, cx);
        cx.notify();
    }

    // What is drawn.

    /// The form both the first-run screen and the Privacy page draw: the two presets, Customize
    /// with each choice and option, the facts that always hold, what a choice would touch and the
    /// switch for erasing the addresses on file, and what the server said no to. `prefix` names
    /// the elements, so the same form can be on two screens.
    pub(super) fn tiny_setup_form(&self, prefix: &'static str, presets_label: &'static str, setup: &TinySetup, cx: &mut Context<Self>) -> Div {
        let state = &self.tiny_state.setup;
        let form = &state.form;
        let id = |part: &str, ix: usize| SharedString::from(format!("{prefix}-{part}-{ix}"));
        let mut body = div().w_full().flex().flex_col().gap(px(24.));

        // The presets: the main choice.
        let mut presets = section(presets_label).gap(px(10.));
        for (ix, preset) in setup.presets.iter().enumerate() {
            let on = form.preset.as_deref() == Some(preset.id.as_str());
            let preset_id = preset.id.clone();
            let mut words = div().flex_1().min_w_0().flex().flex_col().gap(px(3.)).child(title(preset.title.clone()));
            // What a preset means is said once it is picked, so the choice stays short to read.
            if on || !preset.available {
                words = words.child(about(preset.summary.clone()));
            }
            if !preset.available && !preset.unavailable.is_empty() {
                words = words.child(div().whitespace_normal().type_small().text_color(theme::warning()).child(preset.unavailable.clone()));
            }
            let card = pick_card(id("preset", ix), on, preset.available).child(radio(on)).child(words);
            presets = presets.child(if preset.available { card.on_click(cx.listener(move |this, _, _, cx| this.tiny_setup_pick_preset(&preset_id, cx))) } else { card });
        }
        body = body.child(presets);

        // Customize: every choice with its options, as the server words them.
        let customizing = state.customizing;
        body = body.child(
            div().flex().child(
                button(SharedString::from(format!("{prefix}-customize")), ButtonKind::Secondary, if customizing { "Hide the choices" } else { "Customize" })
                    .on_click(cx.listener(|this, _, _, cx| this.tiny_setup_toggle_customize(cx))),
            ),
        );
        if customizing {
            let mut choices = div().w_full().flex().flex_col().gap(px(16.));
            for (cx_ix, choice) in setup.choices.iter().enumerate() {
                let mut card = div().w_full().flex().flex_col().gap(px(8.)).when(!choice.relevant, |card| card.opacity(0.6));
                card = card.child(div().flex().flex_col().gap(px(2.)).child(title(choice.title.clone())).child(about(choice.question.clone())));
                if !choice.relevant {
                    card = card.child(div().type_small().text_color(theme::text_faint()).child("This doesn't apply to this server."));
                }
                for (ix, option) in choice.options.iter().enumerate() {
                    let on = form.is_picked(&choice.id, &option.id);
                    let blocked = tiny_setup::blocked(setup, &option.needs);
                    let (choice_id, option_id) = (choice.id.clone(), option.id.clone());
                    let mut words = div().flex_1().min_w_0().flex().flex_col().gap(px(3.)).child(title(option.title.clone()));
                    if on {
                        words = words.child(about(option.effect.clone()));
                    }
                    if let Some(why) = blocked {
                        words = words.child(div().whitespace_normal().type_small().text_color(theme::warning()).child(why));
                    } else if on && tiny_setup::hint(setup, &option.needs) == Some(Hint::AddPasskey) {
                        let hint = div().whitespace_normal().type_small().text_color(theme::warning()).child(tiny_setup::ADD_PASSKEY_WORDS);
                        let add = if state.linking {
                            button_disabled(tiny_setup::remedy_label(Remedy::AddPasskey)).into_any_element()
                        } else {
                            button(SharedString::from(format!("{prefix}-hint-link")), ButtonKind::Secondary, tiny_setup::remedy_label(Remedy::AddPasskey))
                                .on_click(cx.listener(|this, _, _, cx| this.tiny_setup_link(cx)))
                                .into_any_element()
                        };
                        words = words.child(hint).child(div().mt(px(4.)).flex().child(add));
                    }
                    let row = pick_card(id(&format!("choice{cx_ix}"), ix), on, blocked.is_none()).child(radio(on)).child(words);
                    card = card.child(if blocked.is_none() { row.on_click(cx.listener(move |this, _, _, cx| this.tiny_setup_pick_option(&choice_id, &option_id, cx))) } else { row });
                }
                choices = choices.child(card);
            }
            body = body.child(choices);
        }

        // What always holds, what a choice would touch and the switch for erasing the addresses on
        // file: with the choices, so the first screen stays two cards.
        if customizing && !setup.always.is_empty() {
            let mut always = div().w_full().flex().flex_col().gap(px(8.));
            for line in &setup.always {
                always = always.child(aside("icons/info.svg", theme::text_faint(), line.clone()));
            }
            body = body.child(always);
        }

        // What a choice would touch, before the owner commits, and the one thing it can erase.
        let counts = tiny_setup::counts_words(setup);
        let erasable = setup.counts.emails_stored > 0;
        if customizing && (!counts.is_empty() || erasable) {
            let mut touch = section("What this touches").gap(px(8.));
            for line in counts {
                touch = touch.child(div().whitespace_normal().type_ui().text_color(theme::text()).child(line));
            }
            if erasable {
                let on = form.erase_emails;
                touch = touch.child(
                    div()
                        .id(SharedString::from(format!("{prefix}-erase")))
                        .mt(px(4.))
                        .flex()
                        .items_start()
                        .gap(px(10.))
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, _, cx| this.tiny_setup_toggle_erase(cx)))
                        .child(div().mt(px(2.)).child(check_box(SharedString::from(format!("{prefix}-erase-box")), Some(on), true)))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap(px(2.))
                                .child(title(tiny_setup::ERASE_LABEL))
                                .child(div().whitespace_normal().type_small().text_color(if on { theme::danger() } else { theme::text_soft() }).child(tiny_setup::ERASE_WARNING)),
                        ),
                );
            }
            body = body.child(touch);
        }

        // What the server said no to, and what can be done.
        if let Some(refusal) = &state.refusal {
            body = body.child(self.tiny_setup_refusal(prefix, refusal, cx));
        }
        if let Some(detail) = &state.error {
            body = body.child(aside("icons/circle-alert.svg", theme::danger(), detail.clone()));
        }
        // What came of asking for a passkey link, with a way to read what the server counts again.
        if let Some(note) = &state.link_note {
            body = body.child(
                div().w_full().flex().flex_col().gap(px(10.)).child(aside("icons/info.svg", theme::text_faint(), note.clone())).child(
                    div().flex().child(
                        button(SharedString::from(format!("{prefix}-check")), ButtonKind::Secondary, "Check again").on_click(cx.listener(|this, _, _, cx| this.tiny_setup_check_again(cx))),
                    ),
                ),
            );
        }
        body
    }

    fn tiny_setup_refusal(&self, prefix: &'static str, refusal: &TinySetupRefusal, cx: &mut Context<Self>) -> Div {
        let state = &self.tiny_state.setup;
        let panel = div().w_full().flex().flex_col().gap(px(10.)).child(aside("icons/triangle-alert.svg", theme::warning(), tiny_setup::refusal_words(refusal)));
        let mut actions = div().flex().gap(px(8.));
        if let Some(remedy) = tiny_setup::remedy(refusal) {
            let label = tiny_setup::remedy_label(remedy);
            let busy = state.sending || (remedy == Remedy::AddPasskey && state.linking);
            actions = actions.child(if busy {
                button_disabled(label).into_any_element()
            } else {
                button(SharedString::from(format!("{prefix}-remedy")), if remedy == Remedy::DoItAnyway { ButtonKind::Danger } else { ButtonKind::Primary }, label)
                    .on_click(cx.listener(move |this, _, _, cx| this.tiny_setup_remedy(remedy, cx)))
                    .into_any_element()
            });
        }
        panel.child(actions)
    }

    /// The first-run screen: a full-window page over the server, as Server Settings is, so it has
    /// the room to say what each choice does. "Later" and Escape put it away.
    pub(super) fn tiny_setup_overlay(&self, page: &Page, cx: &mut Context<Self>) -> AnyElement {
        let state = &self.tiny_state.setup;
        let Some(setup) = state.data.as_ref() else {
            return div().into_any_element();
        };
        let form = &state.form;
        let heading = div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .child(div().type_headline().text_color(theme::text()).child(format!("Set up {}", page.name)))
            .child(about("Choose what this server keeps about its members. You can change it later in Server Settings › Privacy."));
        let apply_label: SharedString = match form.preset.as_deref().and_then(|id| setup.presets.iter().find(|preset| preset.id == id)) {
            Some(preset) if !state.customizing => format!("Use {}", preset.title).into(),
            _ => "Apply".into(),
        };
        let apply = if state.sending {
            button_disabled("Applying…").into_any_element()
        } else if !form.answers(setup) {
            button_disabled(apply_label).into_any_element()
        } else {
            button("tiny-setup-apply", ButtonKind::Primary, apply_label).on_click(cx.listener(|this, _, _, cx| this.tiny_setup_send(Answer::Form, cx))).into_any_element()
        };
        let keep = if state.sending {
            button_disabled("Keep what I have").into_any_element()
        } else {
            button("tiny-setup-keep", ButtonKind::Secondary, "Keep what I have").on_click(cx.listener(|this, _, _, cx| this.tiny_setup_send(Answer::Keep, cx))).into_any_element()
        };
        let footer = div()
            .flex_none()
            .w_full()
            .px(px(40.))
            .py(px(14.))
            .border_t_1()
            .border_color(theme::hairline())
            .bg(theme::sidebar())
            .flex()
            .justify_center()
            .child(
                div()
                    .w_full()
                    .max_w(px(680.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(button("tiny-setup-later", ButtonKind::Ghost, "Later").on_click(cx.listener(|this, _, _, cx| this.tiny_setup_later(cx))))
                    .child(div().flex_1())
                    .child(keep)
                    .child(apply),
            );
        div()
            .id("tiny-setup")
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .bg(theme::canvas())
            .occlude()
            .flex()
            .flex_col()
            .font_family(theme::FONT)
            .text_color(theme::text())
            .child(
                div().id("tiny-setup-scroll").flex_1().min_h_0().w_full().overflow_y_scroll().flex().justify_center().child(
                    div().w_full().max_w(px(760.)).px(px(40.)).pt(px(64.)).pb(px(32.)).flex().flex_col().gap(px(28.)).child(heading).child(self.tiny_setup_form("tiny-setup", "Start from", setup, cx)),
                ),
            )
            .child(footer)
            .into_any_element()
    }
}

/// The outline a server with passkeys and one sign-in provider would send, with an owner who has
/// no passkey yet and three members who have none, for `--tiny-setup`.
#[cfg(feature = "gui-shot")]
const SHOT_OUTLINE: &str = r#"{
  "needed": true, "completed_at": null,
  "current": {"provider_profile": "full", "activity_visibility": "everyone", "sign_in_methods": "any"},
  "deployment": {"passkeys": true, "sso": true},
  "counts": {"members": 8, "members_without_passkey": 3, "emails_stored": 8, "owner_has_passkey": false},
  "presets": [
    {"id": "convenience", "title": "Convenience", "summary": "Easiest to run, and how the server worked before this setup. Names and email addresses from sign-in are kept, everyone sees when others were last online, and any sign-in method works.",
     "choices": {"provider_profile": "full", "activity_visibility": "everyone", "sign_in_methods": "any"}, "available": true},
    {"id": "privacy", "title": "Privacy", "summary": "Keeps the least about members. Nothing is kept from a sign-in provider, nobody's last-online time is recorded, and members sign in with passkeys only.",
     "choices": {"provider_profile": "none", "activity_visibility": "nobody", "sign_in_methods": "passkey"}, "available": true}
  ],
  "choices": [
    {"id": "provider_profile", "title": "What to keep from a sign-in provider", "question": "When someone first joins through single sign-on, what should the server keep from their account there?", "relevant": true,
     "options": [
       {"id": "full", "title": "Name and email address", "effect": "Keeps the name and email address the provider gives. People who manage the server can see members' email addresses."},
       {"id": "name", "title": "Name only", "effect": "Keeps the name and never an email address."},
       {"id": "none", "title": "Nothing", "effect": "Keeps neither. New members start as \u201cMember\u201d and choose their own name."}]},
    {"id": "activity_visibility", "title": "Who sees when members were last online", "question": "Should members, or only admins, see when someone was last online and how they joined (by invite or through a group)?", "relevant": true,
     "options": [
       {"id": "everyone", "title": "Every member", "effect": "Everyone sees when others were last online and how they joined."},
       {"id": "admins", "title": "Admins only", "effect": "Only people who manage the server see it."},
       {"id": "nobody", "title": "Nobody", "effect": "The server stops recording when anyone was last online and clears what it has. No one sees how a member joined."}]},
    {"id": "sign_in_methods", "title": "How members sign in", "question": "Which sign-in methods should the server accept?", "relevant": true,
     "options": [
       {"id": "any", "title": "Whatever this server offers", "effect": "Members sign in with a passkey or with single sign-on."},
       {"id": "passkey", "title": "Passkeys only", "effect": "Members sign in with a passkey and nothing else. People who only have a single sign-on account cannot sign in until you give them a link to add a passkey. Sessions already open carry on.", "needs": ["passkeys", "owner_has_passkey"]}]}
  ],
  "always": ["There are no usernames. A member has a display name, which is not unique, and signs in with a passkey or a single sign-on account.", "The server sends no analytics or crash reports anywhere."]
}"#;

#[cfg(feature = "gui-shot")]
impl Shell {
    /// `--tiny-setup first|custom|refused|nopasskeys|privacy|change|member`: the first-run screen (`custom` with
    /// Privacy picked and the choices open, `refused` after a refusal), Server Settings › Privacy
    /// as an owner reads it, or as a member does, over a made-up outline. Nothing is asked of a
    /// server.
    pub(super) fn tiny_shot_setup(&mut self, how: &str, window: &mut gpui::Window, cx: &mut Context<Self>) {
        let owner = how != "member";
        let first = matches!(how, "first" | "custom" | "refused" | "nopasskeys");
        let Some(community) = self.tiny_state.community.as_mut() else {
            return;
        };
        community.capabilities.features.push(protocol::TINY_FEATURE_FIRST_RUN_SETUP.to_owned());
        if owner {
            community.access = Some(protocol::TinyAccess { owner: true, ..community.access.clone().unwrap_or_default() });
        }
        if let Some(server) = community.server.as_mut() {
            server.provider_profile = if how == "member" { "name".into() } else { "full".into() };
            server.activity_visibility = if how == "member" { "admins".into() } else { "everyone".into() };
            server.sign_in_methods = if how == "member" { "passkey".into() } else { "any".into() };
            server.setup_completed_at = if first { String::new() } else { "2026-10-30T09:00:00Z".into() };
        }
        let mut setup: TinySetup = serde_json::from_str(SHOT_OUTLINE).expect("the outline reads");
        setup.needed = first;
        if how == "nopasskeys" {
            // A server started without passkey sign-in: Privacy cannot be chosen as it stands.
            setup.deployment.passkeys = false;
            setup.counts.members_without_passkey = 0;
            if let Some(preset) = setup.presets.iter_mut().find(|preset| preset.id == "privacy") {
                preset.available = false;
                preset.unavailable = "This server was started without passkey sign-in. Choose Privacy with \"How members sign in\" left as it is to take the rest.".into();
            }
            if let Some(choice) = setup.choices.iter_mut().find(|choice| choice.id == "sign_in_methods") {
                choice.relevant = false;
            }
        }
        let state = &mut self.tiny_state.setup;
        state.form = Form::starting(&setup);
        state.customizing = how == "nopasskeys";
        state.changing = how == "change";
        if matches!(how, "custom" | "refused") {
            state.form.pick_preset(&setup, "privacy");
            state.customizing = how == "custom";
        }
        if how == "refused" {
            state.refusal = Some(TinySetupRefusal::MembersWithoutPasskey { count: setup.counts.members_without_passkey });
        }
        state.data = Some(setup);
        state.prompt = first;
        if !first {
            self.tiny_open_settings(super::settings::Page::Privacy, None, window, cx);
        }
    }
}