//! Overview: the server's name, icon, description and welcome, the channel new members land in,
//! the AFK channel with how long until someone idle is moved there, and how large an upload may
//! be. Read-only to whoever may not change the server (and on a server too old to have these, it says so).
//!
//! The icon is sent the moment a picture is chosen (or removed), as Discord does; the rest waits
//! for Save.

use super::kit::switch_row;
use super::{page_head, section, Overlay};
use crate::gui::components::{button, button_disabled, button_with_icon, field, glyph, menu, menu_item_check, segment, segmented, Avatar, ButtonKind, Shape};
use crate::gui::input::TextInput;
use crate::gui::state::Shell;
use crate::gui::theme::{self, Type};
use crate::gui::tiny::Page as Facts;
use crate::client::ask;
use gpui::{anchored, deferred, div, prelude::*, px, AnyElement, App, Context, Div, Entity, Focusable, PathPromptOptions, SharedString, Window};
use protocol::{Request, Response, TinyServerEdit, TinyServerInfo};

/// AFK timeouts offered, in seconds; 0 is off.
const AFK: [(&str, i32); 6] = [("Off", 0), ("1 min", 60), ("5 min", 300), ("15 min", 900), ("30 min", 1_800), ("1 hour", 3_600)];

/// Which room list is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::gui) enum Pick {
    Default,
    Afk,
}

/// What has changed on the page and not been saved. `None` is as the server has it.
#[derive(Default)]
pub(in crate::gui) struct Draft {
    pub default_room: Option<Option<String>>,
    pub afk_room: Option<Option<String>>,
    pub afk_timeout: Option<i32>,
    pub picking: Option<Pick>,
    pub uploading: bool,
    pub icon_error: Option<String>,
    /// `None` is the server's flag. Set when the archives switch is moved.
    pub allow_archives: Option<bool>,
}

fn server_of(facts: &Facts) -> TinyServerInfo {
    facts.community.server.clone().unwrap_or_default()
}

/// The edit Save sends: only what differs from the server.
pub(in crate::gui) fn edit(server: &TinyServerInfo, draft: &Draft, name: &str, description: &str, welcome: &str, limits: bool, image: &str, file: &str, scans: bool) -> TinyServerEdit {
    let changed = |now: &str, typed: &str| (now != typed).then(|| typed.to_owned());
    let cap = |now: i64, typed: &str| {
        if !limits {
            return None;
        }
        let bytes = limit_bytes(typed, now).ok()?;
        (bytes != now).then_some(bytes)
    };
    TinyServerEdit {
        name: changed(&server.name, name.trim()),
        description: changed(&server.description, description.trim()),
        welcome: changed(&server.welcome, welcome.trim()),
        // The server takes an empty id as "none".
        default_room_id: draft.default_room.clone().filter(|room| *room != server.default_room_id).map(Option::unwrap_or_default),
        afk_room_id: draft.afk_room.clone().filter(|room| *room != server.afk_room_id).map(Option::unwrap_or_default),
        afk_timeout_secs: draft.afk_timeout.filter(|secs| *secs != server.afk_timeout_secs),
        image_max_bytes: cap(server.image_max_bytes, image),
        file_max_bytes: cap(server.file_max_bytes, file),
        allow_archives: scans.then(|| draft.allow_archives).flatten().filter(|on| *on != server.allow_archives),
    }
}

/// What the upload field shows. A whole number of mebibytes when that is exact, otherwise the bytes.
fn limit_text(bytes: i64) -> String {
    let unit = 1024 * 1024;
    if bytes > 0 && bytes % unit == 0 && (1..=100).contains(&(bytes / unit)) { (bytes / unit).to_string() } else { bytes.to_string() }
}

/// `1` to `100` are mebibytes. The current byte count, when it is not a whole mebibyte, is left as it is.
fn limit_bytes(text: &str, now: i64) -> Result<i64, ()> {
    let n: i64 = text.trim().parse().map_err(|_| ())?;
    let unit = 1024 * 1024;
    if (1..=100).contains(&n) {
        return Ok(n * unit);
    }
    if n == now && (1..=protocol::TINY_UPLOAD_MAX).contains(&n) {
        return Ok(n);
    }
    Err(())
}

fn size_words(bytes: i64) -> String {
    let unit = 1024 * 1024;
    if bytes > 0 && bytes % unit == 0 { format!("{} MiB", bytes / unit) } else { format!("{bytes} bytes") }
}

fn empty(edit: &TinyServerEdit) -> bool {
    *edit == TinyServerEdit::default()
}

fn facts_limits_invalid(shell: &Shell, cx: &App) -> bool {
    let Some(facts) = shell.tiny_page_facts() else {
        return false;
    };
    if !facts.community.has(protocol::TINY_FEATURE_UPLOAD_LIMITS) {
        return false;
    }
    let server = server_of(&facts);
    let fields = &shell.tiny_state.settings_fields;
    limit_bytes(&fields.image_max.read(cx).text(), server.image_max_bytes).is_err() || limit_bytes(&fields.file_max.read(cx).text(), server.file_max_bytes).is_err()
}

impl Shell {
    fn tiny_overview_edit(&self, cx: &App) -> Option<TinyServerEdit> {
        let facts = self.tiny_page_facts()?;
        let overlay = self.tiny_state.settings.as_ref()?;
        let fields = &self.tiny_state.settings_fields;
        let text = |input: &Entity<TextInput>| input.read(cx).text().to_owned();
        let limits = facts.community.has(protocol::TINY_FEATURE_UPLOAD_LIMITS);
        let scans = facts.community.has(protocol::TINY_FEATURE_FILE_SCAN);
        Some(edit(&server_of(&facts), &overlay.overview, &text(&fields.name), &text(&fields.description), &text(&fields.welcome), limits, &text(&fields.image_max), &text(&fields.file_max), scans))
    }

    pub(in crate::gui) fn tiny_overview_dirty(&self, cx: &App) -> bool {
        let Some(facts) = self.tiny_page_facts() else {
            return false;
        };
        if !facts.may.edit_server {
            return false;
        }
        self.tiny_overview_edit(cx).is_some_and(|edit| !empty(&edit)) || self.tiny_limits_dirty(&facts, cx)
    }

    fn tiny_limits_dirty(&self, facts: &Facts, cx: &App) -> bool {
        if !facts.community.has(protocol::TINY_FEATURE_UPLOAD_LIMITS) {
            return false;
        }
        let server = server_of(facts);
        let fields = &self.tiny_state.settings_fields;
        let off = |text: &str, now: i64| match limit_bytes(text, now) {
            Ok(bytes) => bytes != now,
            Err(()) => true,
        };
        off(&fields.image_max.read(cx).text(), server.image_max_bytes) || off(&fields.file_max.read(cx).text(), server.file_max_bytes)
    }

    pub(in crate::gui) fn tiny_overview_save(&mut self, cx: &mut Context<Self>) {
        let (Some(edit), Some(account)) = (self.tiny_overview_edit(cx), self.tiny_state.account.clone()) else {
            return;
        };
        if facts_limits_invalid(self, cx) {
            if let Some(overlay) = self.tiny_state.settings.as_mut() {
                overlay.error = Some("An upload limit is 1 to 100 MiB.".to_owned());
            }
            cx.notify();
            return;
        }
        if edit.name.as_deref().is_some_and(|name| name.is_empty() || name.chars().count() > protocol::TINY_SERVER_NAME_MAX) {
            if let Some(overlay) = self.tiny_state.settings.as_mut() {
                overlay.error = Some("A server name is 1 to 80 characters.".to_owned());
            }
            cx.notify();
            return;
        }
        if let Some(overlay) = self.tiny_state.settings.as_mut() {
            overlay.saving = true;
        }
        self.act(Request::TinyServerUpdate { account, edit }, cx);
    }

    /// The fields back to what the server holds, and the draft emptied.
    pub(in crate::gui) fn tiny_overview_reset(&mut self, cx: &mut Context<Self>) {
        let server = self.tiny_state.community.as_ref().and_then(|community| community.server.clone()).unwrap_or_default();
        let fields = &self.tiny_state.settings_fields;
        let (name, description, welcome) = (fields.name.clone(), fields.description.clone(), fields.welcome.clone());
        name.update(cx, |input, cx| input.set_text(&server.name, cx));
        description.update(cx, |input, cx| input.set_text(&server.description, cx));
        welcome.update(cx, |input, cx| input.set_text(&server.welcome, cx));
        let image = limit_text(server.image_max_bytes);
        let file = limit_text(server.file_max_bytes);
        fields.image_max.update(cx, |input, cx| input.set_text(&image, cx));
        fields.file_max.update(cx, |input, cx| input.set_text(&file, cx));
        if let Some(overlay) = self.tiny_state.settings.as_mut() {
            overlay.overview = Default::default();
        }
    }

    fn tiny_overview_draft(&mut self, cx: &mut Context<Self>, change: impl FnOnce(&mut Overlay)) {
        if let Some(overlay) = self.tiny_state.settings.as_mut() {
            change(overlay);
            overlay.saved = false;
        }
        cx.notify();
    }

    /// The system's file panel; the picture chosen goes to the server at once.
    fn tiny_pick_icon(&mut self, cx: &mut Context<Self>) {
        let asked = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: false, prompt: Some("Use as icon".into()) });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = asked.await {
                if let Some(path) = paths.into_iter().next() {
                    let _ = this.update(cx, |this, cx| this.tiny_send_icon(Some(path.to_string_lossy().into_owned()), cx));
                }
            }
        })
        .detach();
    }

    fn tiny_send_icon(&mut self, path: Option<String>, cx: &mut Context<Self>) {
        let Some(account) = self.tiny_state.account.clone() else {
            return;
        };
        self.tiny_overview_draft(cx, |overlay| {
            overlay.overview.uploading = true;
            overlay.overview.icon_error = None;
        });
        self.off_thread(cx, move || ask(&Request::TinyServerIcon { account, path }), |this, answer, cx| {
            let failed = match answer {
                Ok(Response::Ok) => None,
                Ok(Response::Error { detail, .. }) => Some(detail),
                Ok(_) => Some(crate::client::OTHER_ANSWER.to_owned()),
                Err(err) => Some(crate::client::trouble_words(&err)),
            };
            if let Some(overlay) = this.tiny_state.settings.as_mut() {
                overlay.overview.uploading = false;
                overlay.overview.icon_error = failed;
            }
            if let Some(account) = this.tiny_state.account.clone() {
                this.fetch(account, None, cx);
            }
            cx.notify();
        });
    }
}

fn input(input: &Entity<TextInput>, tall: bool, window: &Window, cx: &mut Context<Shell>) -> Div {
    let focused = input.focus_handle(cx).is_focused(window);
    let field = field(focused).w_full().child(div().flex_1().min_w_0().child(input.clone()));
    if tall {
        field.h_auto().min_h(px(84.)).py(px(6.)).items_start()
    } else {
        field.h(px(36.))
    }
}

fn counter(text: &str, max: usize) -> Div {
    let used = text.chars().count();
    div().type_small().text_color(if used > max { theme::danger() } else { theme::text_faint() }).child(format!("{used} / {max}"))
}

fn limit_field(label: &'static str, input_entity: &Entity<TextInput>, window: &Window, cx: &mut Context<Shell>) -> Div {
    div().flex_1().flex().flex_col().gap(px(6.)).child(div().type_small().text_color(theme::text_faint()).child(label)).child(input(input_entity, false, window, cx))
}

fn read_only(words: &str) -> Div {
    div()
        .p(px(12.))
        .rounded(px(theme::R_CONTROL))
        .bg(theme::raised())
        .whitespace_normal()
        .type_ui()
        .text_color(if words.is_empty() { theme::text_faint() } else { theme::text() })
        .child(if words.is_empty() { "Not set".to_owned() } else { words.to_owned() })
}

/// A room picker: the room chosen, which opens the list of rooms.
fn room_picker(shell: &Shell, facts: &Facts, pick: Pick, chosen: Option<String>, cx: &mut Context<Shell>) -> Div {
    let name = chosen.as_ref().and_then(|id| facts.community.rooms.iter().find(|room| room.id == *id)).map(|room| room.name.clone());
    let open = shell.tiny_state.settings.as_ref().is_some_and(|overlay| overlay.overview.picking == Some(pick));
    let id = SharedString::from(format!("tiny-pick-{pick:?}"));
    let trigger = div()
        .id(id)
        .w(px(280.))
        .h(px(36.))
        .px(px(10.))
        .rounded(px(theme::R_CONTROL))
        .bg(theme::field())
        .border_1()
        .border_color(if open { theme::focus() } else { theme::border() })
        .hover(|style| style.border_color(theme::border_strong()))
        .flex()
        .items_center()
        .gap(px(8.))
        .cursor_pointer()
        .on_click(cx.listener(move |this, _, _, cx| {
            this.tiny_overview_draft(cx, |overlay| overlay.overview.picking = if open { None } else { Some(pick) });
        }))
        .child(glyph(if name.is_some() { "icons/volume-2.svg" } else { "icons/minus.svg" }, 14., theme::text_faint()))
        .child(div().flex_1().type_ui().text_color(if name.is_some() { theme::text() } else { theme::text_soft() }).child(name.unwrap_or_else(|| "No channel".to_owned())))
        .child(glyph("icons/chevron-down.svg", 14., theme::text_faint()));
    let mut wrap = div().flex().flex_col().child(trigger);
    if open {
        let mut list = menu().w(px(280.)).mt(px(4.)).child(menu_item_check("tiny-pick-none", chosen.is_none(), "No channel", None).on_click(cx.listener(move |this, _, _, cx| {
            this.tiny_overview_draft(cx, |overlay| {
                match pick {
                    Pick::Default => overlay.overview.default_room = Some(None),
                    Pick::Afk => overlay.overview.afk_room = Some(None),
                }
                overlay.overview.picking = None;
            });
        })));
        for (ix, room) in facts.community.rooms.iter().enumerate() {
            let room_id = room.id.clone();
            list = list.child(menu_item_check(("tiny-pick-room", ix), chosen.as_deref() == Some(room.id.as_str()), room.name.clone(), None).on_click(cx.listener(move |this, _, _, cx| {
                let room_id = room_id.clone();
                this.tiny_overview_draft(cx, |overlay| {
                    match pick {
                        Pick::Default => overlay.overview.default_room = Some(Some(room_id)),
                        Pick::Afk => overlay.overview.afk_room = Some(Some(room_id)),
                    }
                    overlay.overview.picking = None;
                });
            })));
        }
        let list = list.on_mouse_down_out(cx.listener(|this, _, _, cx| this.tiny_overview_draft(cx, |overlay| overlay.overview.picking = None)));
        wrap = wrap.child(deferred(anchored().snap_to_window().child(list)).with_priority(2));
    }
    wrap
}

pub(in crate::gui::tiny) fn overview(shell: &Shell, facts: &Facts, window: &mut Window, cx: &mut Context<Shell>) -> AnyElement {
    let server = server_of(facts);
    let editable = facts.may.edit_server;
    let known = facts.community.has(protocol::TINY_FEATURE_SERVER);
    let Some(overlay) = shell.tiny_state.settings.as_ref() else {
        return div().into_any_element();
    };
    let draft = &overlay.overview;
    let fields = &shell.tiny_state.settings_fields;
    let about = if !known {
        "This server is older than server settings. Update tinyserver to name it, give it an icon and a welcome."
    } else if editable {
        "How the server looks to everyone, and where people land."
    } else {
        "How this server describes itself. Only those who manage the server can change it."
    };
    let mut page = div().flex().flex_col().gap(px(28.)).child(page_head("Overview", about));
    if !known {
        return page.into_any_element();
    }

    // The icon, then the name beside it.
    let icon = Avatar::named(&facts.name, 96.).shape(Shape::Place).image_at(facts.icon()).build();
    let icon_actions = if editable {
        let upload = if draft.uploading {
            button_disabled("Uploading…").into_any_element()
        } else {
            button_with_icon("tiny-icon-upload", ButtonKind::Secondary, "icons/upload.svg", "Upload image").on_click(cx.listener(|this, _, _, cx| this.tiny_pick_icon(cx))).into_any_element()
        };
        let remove = (server.icon.is_some() && !draft.uploading).then(|| button("tiny-icon-remove", ButtonKind::Ghost, "Remove").on_click(cx.listener(|this, _, _, cx| this.tiny_send_icon(None, cx))));
        div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(div().flex().gap(px(8.)).child(upload).children(remove))
            .child(div().type_small().text_color(theme::text_faint()).child("PNG, JPEG or WebP, up to 256 KB. Square works best."))
            .children(draft.icon_error.clone().map(|detail| div().whitespace_normal().type_small().text_color(theme::danger()).child(detail)))
            .into_any_element()
    } else {
        div().into_any_element()
    };
    let name_text = fields.name.read(cx).text().to_owned();
    let name = if editable {
        section("Server name").child(input(&fields.name, false, window, cx)).child(counter(&name_text, protocol::TINY_SERVER_NAME_MAX))
    } else {
        section("Server name").child(read_only(&server.name))
    };
    page = page.child(div().flex().gap(px(24.)).items_start().child(div().flex().flex_col().items_center().gap(px(10.)).child(icon)).child(div().flex_1().flex().flex_col().gap(px(16.)).child(name).child(icon_actions)));

    let description_text = fields.description.read(cx).text().to_owned();
    let welcome_text = fields.welcome.read(cx).text().to_owned();
    page = page.child(if editable {
        section("Description").child(input(&fields.description, true, window, cx)).child(counter(&description_text, protocol::TINY_DESCRIPTION_MAX))
    } else {
        section("Description").child(read_only(&server.description))
    });
    page = page.child(if editable {
        section("Welcome message").child(div().type_small().text_color(theme::text_soft()).child("Shown on the server's home, under its name.")).child(input(&fields.welcome, true, window, cx)).child(counter(&welcome_text, protocol::TINY_WELCOME_MAX))
    } else {
        section("Welcome message").child(read_only(&server.welcome))
    });
    let limits = facts.community.has(protocol::TINY_FEATURE_UPLOAD_LIMITS);
    let scans = facts.community.has(protocol::TINY_FEATURE_FILE_SCAN);
    if limits || scans {
        let mut uploads = section("Uploads");
        if scans {
            uploads = uploads.child(div().type_small().text_color(theme::text_soft()).child("Each file is scanned before anyone else can open it."));
        }
        if limits && editable {
            uploads = uploads
                .child(div().type_small().text_color(theme::text_soft()).child("Images and GIFs, then video and other files. Each is 1 to 100 MiB."))
                .child(div().flex().gap(px(16.)).child(limit_field("Images and GIFs", &fields.image_max, window, cx)).child(limit_field("Video and other files", &fields.file_max, window, cx)));
        } else if limits {
            uploads = uploads.child(read_only(&format!("Images and GIFs up to {}. Video and other files up to {}.", size_words(server.image_max_bytes), size_words(server.file_max_bytes))));
        }
        if scans && editable {
            let on = draft.allow_archives.unwrap_or(server.allow_archives);
            uploads = uploads.child(
                switch_row("tiny-allow-archives", "Allow archives", "Zip, 7z, rar, tar and gzip can be sent. Off, they are refused.", on, true).on_click(cx.listener(move |this, _, _, cx| {
                    this.tiny_overview_draft(cx, move |overlay| overlay.overview.allow_archives = Some(!on));
                })),
            );
        }
        page = page.child(uploads);
    }

    let default_room = draft.default_room.clone().unwrap_or(server.default_room_id.clone());
    let afk_room = draft.afk_room.clone().unwrap_or(server.afk_room_id.clone());
    let afk_timeout = draft.afk_timeout.unwrap_or(server.afk_timeout_secs);
    let room_name = |id: &Option<String>| id.as_ref().and_then(|id| facts.community.rooms.iter().find(|room| room.id == *id)).map(|room| room.name.clone()).unwrap_or_default();
    page = page.child(div().h(px(1.)).bg(theme::hairline()));
    page = page.child(if editable {
        section("Default channel").child(div().type_small().text_color(theme::text_soft()).child("Where new members are pointed first. Its Join button is on the home.")).child(room_picker(shell, facts, Pick::Default, default_room, cx))
    } else {
        section("Default channel").child(read_only(&room_name(&default_room)))
    });
    let mut timeouts = segmented();
    for (ix, (words, secs)) in AFK.iter().enumerate() {
        let secs = *secs;
        timeouts = timeouts.child(segment(("tiny-afk", ix), *words, afk_timeout == secs).on_click(cx.listener(move |this, _, _, cx| this.tiny_overview_draft(cx, |overlay| overlay.overview.afk_timeout = Some(secs)))));
    }
    page = page.child(if editable {
        section("AFK channel")
            .child(div().type_small().text_color(theme::text_soft()).child("Someone who hasn't spoken for this long is moved here by their own tinyclient."))
            .child(room_picker(shell, facts, Pick::Afk, afk_room, cx))
            .child(div().pt(px(4.)).child(timeouts))
    } else {
        section("AFK channel").child(read_only(&match (room_name(&afk_room), afk_timeout) {
            (name, _) if name.is_empty() => String::new(),
            (name, 0) => name,
            (name, secs) => format!("{name}, after {}", crate::gui::tiny::model::span_words(i64::from(secs))),
        }))
    });
    if overlay.saved && !shell.tiny_settings_dirty(cx) {
        page = page.child(div().flex().items_center().gap(px(6.)).child(glyph("icons/check.svg", 14., theme::positive())).child(div().type_small().text_color(theme::positive()).child("Saved")));
    }
    page.into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_sends_only_what_changed() {
        let server = TinyServerInfo { name: "Night Shift".into(), description: "Hi".into(), afk_timeout_secs: 300, default_room_id: Some("a".into()), ..TinyServerInfo::default() };
        let draft = Draft::default();
        assert!(empty(&edit(&server, &draft, "Night Shift ", "Hi", "", false, "", "", false)));
        let draft = Draft { default_room: Some(None), afk_timeout: Some(300), afk_room: Some(Some("b".into())), ..Draft::default() };
        let sent = edit(&server, &draft, "Day Shift", "Hi", "Welcome", false, "", "", false);
        assert_eq!(sent.name.as_deref(), Some("Day Shift"));
        assert_eq!(sent.description, None);
        assert_eq!(sent.welcome.as_deref(), Some("Welcome"));
        assert_eq!(sent.default_room_id.as_deref(), Some(""), "no default channel is sent as an empty id");
        assert_eq!(sent.afk_room_id.as_deref(), Some("b"));
        assert_eq!(sent.afk_timeout_secs, None, "unchanged");
        assert_eq!(sent.image_max_bytes, None);
        assert_eq!(sent.file_max_bytes, None);
        let raised = edit(&server, &Draft::default(), "Night Shift", "Hi", "", true, "12", "25", false);
        assert_eq!(raised.image_max_bytes, Some(12 * 1024 * 1024));
        assert_eq!(raised.file_max_bytes, None, "25 MiB is already the file cap");
        let odd = TinyServerInfo { image_max_bytes: 1_500, ..server.clone() };
        let kept = edit(&odd, &Draft::default(), "Night Shift", "Hi", "", true, "1500", "25", false);
        assert_eq!(kept.image_max_bytes, None, "the current byte count is left as it is");
        assert!(empty(&edit(&server, &Draft::default(), "Night Shift", "Hi", "", true, "0", "101", false)));
        let archives = Draft { allow_archives: Some(true), ..Draft::default() };
        assert_eq!(edit(&server, &archives, "Night Shift", "Hi", "", false, "", "", true).allow_archives, Some(true));
        assert_eq!(edit(&server, &archives, "Night Shift", "Hi", "", false, "", "", false).allow_archives, None, "an older server does not send the toggle");
        assert_eq!(edit(&server, &Draft::default(), "Night Shift", "Hi", "", false, "", "", true).allow_archives, None);
    }
}
