//! Encryption's backup file: save what this device has read to a file locked with a passphrase,
//! and take such a file in on a device of the same person. No server keeps it. The passphrase
//! fields show dots, never the letters.

use super::kit::{self, security_card as card};
use super::encryption::{Draft, Visit};
use crate::client::ask;
use crate::gui::components::{button_disabled, button_with_icon, fade_in, ButtonKind};
use crate::gui::input::TextInput;
use crate::gui::state::Shell;
use crate::gui::theme::{self, Type};
use gpui::{div, prelude::*, px, Context, Div, Entity, PathPromptOptions, Window};
use protocol::{E2eeAsk, Request, Response};
use std::path::PathBuf;

/// The shortest passphrase a backup is made with (chatd says the same).
const SHORTEST: usize = 12;
const FILE_NAME: &str = "tinyclient-backup.tnyb";

/// Where the save dialog opens: Downloads, else home.
fn start_folder() -> PathBuf {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(PathBuf::from).unwrap_or_default();
    let downloads = home.join("Downloads");
    if downloads.is_dir() {
        downloads
    } else {
        home
    }
}

/// What is wrong with the passphrase typed twice for a new backup, in words.
pub(in crate::gui) fn passphrase_problem(first: &str, again: &str) -> Option<String> {
    if first.chars().count() < SHORTEST {
        return Some(format!("Use at least {SHORTEST} characters."));
    }
    (first != again).then(|| "The two passphrases aren't the same.".to_owned())
}

/// These are genuine secret TextInputs: only their masks enter shaping and paint.
fn secret(input: &Entity<TextInput>, window: &mut Window, cx: &mut Context<Shell>) -> Div {
    kit::input(input, None, window, cx).min_w_0().max_w(px(360.))
}

/// Applies an action result only to the account and page visit which started it.
fn backup_update(draft: &mut Draft, visit: &Visit, account: Option<&protocol::AccountKey>, visible: bool,
                 note: Option<String>, error: Option<String>, busy: bool) -> bool {
    if !draft.accepts(visit, account, visible) { return false; }
    draft.note = note;
    draft.error = error;
    draft.loading = busy;
    true
}

impl Shell {
    fn tiny_backup_says(&mut self, visit: &Visit, note: Option<String>, error: Option<String>, busy: bool, cx: &mut Context<Self>) -> bool {
        let account = self.tiny_state.account.as_ref();
        let Some(overlay) = self.tiny_state.settings.as_mut() else { return false; };
        if !backup_update(&mut overlay.encryption, visit, account, overlay.page == super::Page::Encryption, note, error, busy) { return false; }
        cx.notify();
        true
    }

    fn tiny_backup_ask(&mut self, visit: Visit, ask_for: E2eeAsk, done: impl Fn(u64) -> String + 'static, cx: &mut Context<Self>) {
        if !self.tiny_backup_says(&visit, None, None, true, cx) { return; }
        let account = visit.account.clone();
        self.off_thread(
            cx,
            move || ask(&Request::TinyE2ee { account, ask: ask_for }),
            move |this, answer, cx| {
                let (note, error) = match answer {
                    Ok(Response::TinyBackup { messages }) => (Some(done(messages)), None),
                    Ok(Response::Error { detail, .. }) => (None, Some(detail)),
                    Ok(_) => (None, Some(crate::client::OTHER_ANSWER.to_owned())),
                    Err(err) => (None, Some(crate::client::trouble_words("tiny", &err))),
                };
                this.tiny_backup_says(&visit, note, error, false, cx);
            },
        );
    }

    fn tiny_backup_export(&mut self, cx: &mut Context<Self>) {
        let Some(visit) = self.tiny_e2ee_visit() else { return; };
        if self.tiny_e2ee_draft_for(&visit).is_none_or(|draft| draft.loading) { return; }
        let fields = &self.tiny_state.settings_fields;
        let first = fields.backup_passphrase.read(cx).text().to_owned();
        let again = fields.backup_again.read(cx).text().to_owned();
        if let Some(problem) = passphrase_problem(&first, &again) {
            self.tiny_backup_says(&visit, None, Some(problem), false, cx);
            return;
        }
        self.tiny_backup_says(&visit, None, None, true, cx);
        let asked = cx.prompt_for_new_path(&start_folder(), Some(FILE_NAME));
        cx.spawn(async move |this, cx| {
            let selected = asked.await;
            let _ = this.update(cx, |this, cx| {
                if this.tiny_e2ee_draft_for(&visit).is_none() { return; }
                let Ok(Ok(Some(path))) = selected else {
                    this.tiny_backup_says(&visit, None, None, false, cx);
                    return;
                };
                for (field, original) in [(this.tiny_state.settings_fields.backup_passphrase.clone(), &first), (this.tiny_state.settings_fields.backup_again.clone(), &again)] {
                    if field.read(cx).text() == original.as_str() {
                        field.update(cx, |input, cx| input.clear(cx));
                    }
                }
                let ask_for = E2eeAsk::ExportBackup { path: path.to_string_lossy().into_owned(), passphrase: first };
                this.tiny_backup_ask(
                    visit,
                    ask_for,
                    |messages| match messages {
                        1 => "Saved, with 1 message. Keep the passphrase: without it the file can't be opened.".to_owned(),
                        n => format!("Saved, with {n} messages. Keep the passphrase: without it the file can't be opened."),
                    },
                    cx,
                );
            });
        })
        .detach();
    }

    fn tiny_backup_import(&mut self, cx: &mut Context<Self>) {
        let Some(visit) = self.tiny_e2ee_visit() else { return; };
        if self.tiny_e2ee_draft_for(&visit).is_none_or(|draft| draft.loading) { return; }
        let passphrase = self.tiny_state.settings_fields.backup_open.read(cx).text().to_owned();
        if passphrase.is_empty() {
            self.tiny_backup_says(&visit, None, Some("Type the backup's passphrase first.".to_owned()), false, cx);
            return;
        }
        self.tiny_backup_says(&visit, None, None, true, cx);
        let asked = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: false, prompt: Some("Import".into()) });
        cx.spawn(async move |this, cx| {
            let selected = asked.await;
            let _ = this.update(cx, |this, cx| {
                if this.tiny_e2ee_draft_for(&visit).is_none() { return; }
                let path = match selected {
                    Ok(Ok(Some(paths))) => paths.into_iter().next(),
                    _ => None,
                };
                let Some(path) = path else {
                    this.tiny_backup_says(&visit, None, None, false, cx);
                    return;
                };
                let field = this.tiny_state.settings_fields.backup_open.clone();
                if field.read(cx).text() == passphrase.as_str() {
                    field.update(cx, |input, cx| input.clear(cx));
                }
                let ask_for = E2eeAsk::ImportBackup { path: path.to_string_lossy().into_owned(), passphrase };
                this.tiny_backup_ask(
                    visit,
                    ask_for,
                    |messages| match messages {
                        0 => "Done. This device already had everything in that backup.".to_owned(),
                        1 => "Done. 1 message came from the backup.".to_owned(),
                        n => format!("Done. {n} messages came from the backup."),
                    },
                    cx,
                );
            });
        })
        .detach();
    }
}

/// The Backup file section.
pub(in crate::gui::tiny) fn backup(shell: &Shell, busy: bool, window: &mut Window, cx: &mut Context<Shell>) -> Div {
    let fields = &shell.tiny_state.settings_fields;
    let mode = shell.tiny_state.settings.as_ref().and_then(|overlay| overlay.encryption.backup_mode);
    #[cfg(feature = "gui-shot")]
    let mode = match std::env::var("TINYCLIENT_SHOT_BACKUP").as_deref() {
        Ok("save") => Some(true),
        Ok("import") => Some(false),
        _ => mode,
    };
    let pick = |save: bool, cx: &mut Context<Shell>| {
        button_with_icon(
            if save { "tiny-e2ee-backup-mode-save" } else { "tiny-e2ee-backup-mode-open" },
            if mode == Some(save) { ButtonKind::Secondary } else { ButtonKind::Ghost },
            if save { "icons/download.svg" } else { "icons/upload.svg" },
            if save { "Save" } else { "Import" },
        )
        .h(px(36.))
        .on_click(cx.listener(move |this, _, _, cx| {
            if let Some(draft) = this.tiny_state.settings.as_mut().map(|overlay| &mut overlay.encryption) {
                draft.backup_mode = if draft.backup_mode == Some(save) { None } else { Some(save) };
            }
            cx.notify();
        }))
    };
    let mut body = card().p(px(12.)).gap(px(14.)).child(
        div().min_w_0().flex().flex_wrap().gap(px(6.)).child(pick(true, cx)).child(pick(false, cx)),
    );
    if let Some(save) = mode {
        let mut form = div().min_w_0().max_w_full().flex().flex_col().items_start().gap(px(10.));
        if save {
            form = form
                .child(kit::security_words("Keep this passphrase safe. It cannot be reset.").type_small().text_color(theme::text_soft()))
                .child(secret(&fields.backup_passphrase, window, cx))
                .child(secret(&fields.backup_again, window, cx))
                .child(if busy {
                    button_disabled("Saving…").into_any_element()
                } else {
                    button_with_icon("tiny-e2ee-backup-save", ButtonKind::Primary, "icons/download.svg", "Save backup…")
                        .h(px(36.))
                        .on_click(cx.listener(|this, _, _, cx| this.tiny_backup_export(cx))).into_any_element()
                });
        } else {
            form = form
                .child(kit::security_words("Restore messages from one of your backups.").type_small().text_color(theme::text_soft()))
                .child(secret(&fields.backup_open, window, cx))
                .child(if busy {
                    button_disabled("Importing…").into_any_element()
                } else {
                    button_with_icon("tiny-e2ee-backup-open", ButtonKind::Primary, "icons/upload.svg", "Choose backup…")
                        .h(px(36.))
                        .on_click(cx.listener(|this, _, _, cx| this.tiny_backup_import(cx))).into_any_element()
                });
        }
        body = body.child(fade_in(if save { "tiny-backup-save-form" } else { "tiny-backup-open-form" }, 160, form));
    }
    kit::security_section("icons/lock.svg", "Message backup").child(body)

}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backup_lifetime_reply_preserves_a_replacement_account_overlay_or_tab() {
        let one = protocol::AccountKey { network: protocol::Network::Tiny, account: "one".into() };
        let two = protocol::AccountKey { network: protocol::Network::Tiny, account: "two".into() };
        let original = Draft::default();
        let visit = original.visit(&one);
        let mut reopened = Draft::default();
        reopened.note = Some("New visit".into());
        reopened.loading = true;
        assert!(!backup_update(&mut reopened, &visit, Some(&one), true, Some("Old saved".into()), None, false));
        assert_eq!(reopened.note.as_deref(), Some("New visit"));
        assert!(reopened.loading);
        let mut switched = original;
        assert!(!backup_update(&mut switched, &visit, Some(&two), true, None, Some("Old failed".into()), false));
        assert!(switched.error.is_none());
        switched.invalidate_reads();
        assert!(!backup_update(&mut switched, &visit, Some(&one), true, Some("Old tab".into()), None, false));
        let current = switched.visit(&one);
        assert!(!backup_update(&mut switched, &current, Some(&one), false, None, Some("Hidden".into()), false));
        assert!(switched.error.is_none());
    }

    #[test]
    fn backup_lifetime_current_action_completes_with_its_note_error_and_busy_state() {
        let account = protocol::AccountKey { network: protocol::Network::Tiny, account: "one".into() };
        let mut draft = Draft::default();
        let visit = draft.visit(&account);
        assert!(backup_update(&mut draft, &visit, Some(&account), true, None, None, true));
        assert!(draft.loading);
        assert!(backup_update(&mut draft, &visit, Some(&account), true, Some("Saved".into()), None, false));
        assert_eq!(draft.note.as_deref(), Some("Saved"));
        assert!(!draft.loading);
    }

    #[test]
    fn a_backup_passphrase_is_long_enough_and_typed_the_same_twice() {
        assert!(passphrase_problem("short", "short").unwrap().contains("at least 12"));
        assert!(passphrase_problem("a long passphrase", "a long passphrase!").unwrap().contains("aren't the same"));
        assert_eq!(passphrase_problem("a long passphrase", "a long passphrase"), None);
        // Characters, not bytes: twelve letters of any script.
        assert!(passphrase_problem("ççççççççççç", "ççççççççççç").is_some());
        assert_eq!(passphrase_problem("çççççççççççç", "çççççççççççç"), None);
    }
}
