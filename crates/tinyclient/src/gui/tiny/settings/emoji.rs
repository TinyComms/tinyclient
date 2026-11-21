//! Community custom emoji and stickers (`GET/POST/PATCH/DELETE /v1/emoji`), shown under Server
//! Settings to members with `manage_server` on a server that lists `custom_emoji` or `stickers`.

use super::kit::{self, cell, table_head, table_row};
use super::page_head;
use crate::client::ask;
use crate::gui::components::{button, button_with_icon, segment, segmented, ButtonKind};
use crate::gui::state::Shell;
use crate::gui::theme::{self, Type};
use crate::gui::tiny::Page as Facts;
use gpui::{div, prelude::*, px, AnyElement, Context, PathPromptOptions, Window};
use protocol::{Request, Response, TinyEmoji};

#[derive(Default)]
pub(in crate::gui) struct Draft {
    pub items: Vec<TinyEmoji>,
    pub loading: bool,
    pub asked: bool,
    pub error: Option<String>,
    /// `"emoji"` or `"sticker"` for the upload form.
    pub kind: &'static str,
    /// When renaming an existing item, its id.
    pub renaming: Option<String>,
}

impl Draft {
    pub fn kind_or_default(&self) -> &'static str {
        if self.kind == "sticker" { "sticker" } else { "emoji" }
    }
}

impl Shell {
    pub(in crate::gui) fn tiny_emoji_load(&mut self, cx: &mut Context<Self>) {
        let Some(account) = self.tiny_state.account.clone() else {
            return;
        };
        let Some(draft) = self.tiny_state.settings.as_mut().map(|o| &mut o.emoji) else {
            return;
        };
        draft.asked = true;
        draft.loading = true;
        draft.error = None;
        cx.notify();
        self.off_thread(
            cx,
            move || ask(&Request::TinyEmojiList { account }),
            move |this, answer, cx| {
                let Some(draft) = this.tiny_state.settings.as_mut().map(|o| &mut o.emoji) else {
                    return;
                };
                draft.loading = false;
                match answer {
                    Ok(Response::TinyEmojiList { items }) => draft.items = items,
                    Ok(Response::Error { detail, .. }) => draft.error = Some(detail),
                    Ok(_) => draft.error = Some(crate::client::OTHER_ANSWER.to_owned()),
                    Err(err) => draft.error = Some(crate::client::trouble_words("tiny", &err)),
                }
                cx.notify();
            },
        );
    }

    pub(in crate::gui) fn tiny_emoji_upload(&mut self, cx: &mut Context<Self>) {
        let Some(account) = self.tiny_state.account.clone() else {
            return;
        };
        let name = self.tiny_state.settings_fields.emoji_name.read(cx).text().trim().to_owned();
        let kind = self.tiny_state.settings.as_ref().map(|o| o.emoji.kind_or_default()).unwrap_or("emoji").to_owned();
        let pick = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Choose an emoji or sticker image".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = pick.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let path_str = path.to_string_lossy().into_owned();
            let _ = this.update(cx, |this, cx| {
                if let Some(draft) = this.tiny_state.settings.as_mut().map(|o| &mut o.emoji) {
                    draft.loading = true;
                    draft.error = None;
                }
                cx.notify();
                this.off_thread(
                    cx,
                    move || ask(&Request::TinyEmojiCreate { account, name, kind, path: path_str }),
                    move |this, answer, cx| {
                        let reload = {
                            let Some(draft) = this.tiny_state.settings.as_mut().map(|o| &mut o.emoji) else {
                                return;
                            };
                            draft.loading = false;
                            match answer {
                                Ok(Response::Ok) => true,
                                Ok(Response::Error { detail, .. }) => {
                                    draft.error = Some(detail);
                                    false
                                }
                                Ok(_) => {
                                    draft.error = Some(crate::client::OTHER_ANSWER.to_owned());
                                    false
                                }
                                Err(err) => {
                                    draft.error = Some(crate::client::trouble_words("tiny", &err));
                                    false
                                }
                            }
                        };
                        if reload {
                            this.tiny_state.settings_fields.emoji_name.update(cx, |i, cx| i.set_text("", cx));
                            this.tiny_emoji_load(cx);
                        }
                        cx.notify();
                    },
                );
            });
        })
        .detach();
    }

    pub(in crate::gui) fn tiny_emoji_rename(&mut self, emoji_id: String, cx: &mut Context<Self>) {
        let Some(account) = self.tiny_state.account.clone() else {
            return;
        };
        let name = self.tiny_state.settings_fields.emoji_name.read(cx).text().trim().to_owned();
        if let Some(draft) = self.tiny_state.settings.as_mut().map(|o| &mut o.emoji) {
            draft.loading = true;
            draft.error = None;
        }
        cx.notify();
        self.off_thread(
            cx,
            move || ask(&Request::TinyEmojiRename { account, emoji_id, name }),
            move |this, answer, cx| {
                let reload = {
                    let Some(draft) = this.tiny_state.settings.as_mut().map(|o| &mut o.emoji) else {
                        return;
                    };
                    draft.loading = false;
                    match answer {
                        Ok(Response::Ok) => {
                            draft.renaming = None;
                            true
                        }
                        Ok(Response::Error { detail, .. }) => {
                            draft.error = Some(detail);
                            false
                        }
                        Ok(_) => {
                            draft.error = Some(crate::client::OTHER_ANSWER.to_owned());
                            false
                        }
                        Err(err) => {
                            draft.error = Some(crate::client::trouble_words("tiny", &err));
                            false
                        }
                    }
                };
                if reload {
                    this.tiny_state.settings_fields.emoji_name.update(cx, |i, cx| i.set_text("", cx));
                    this.tiny_emoji_load(cx);
                }
                cx.notify();
            },
        );
    }

    pub(in crate::gui) fn tiny_emoji_delete(&mut self, emoji_id: String, cx: &mut Context<Self>) {
        let Some(account) = self.tiny_state.account.clone() else {
            return;
        };
        if let Some(draft) = self.tiny_state.settings.as_mut().map(|o| &mut o.emoji) {
            draft.loading = true;
            draft.error = None;
        }
        cx.notify();
        self.off_thread(
            cx,
            move || ask(&Request::TinyEmojiDelete { account, emoji_id }),
            move |this, answer, cx| {
                let reload = {
                    let Some(draft) = this.tiny_state.settings.as_mut().map(|o| &mut o.emoji) else {
                        return;
                    };
                    draft.loading = false;
                    match answer {
                        Ok(Response::Ok) => true,
                        Ok(Response::Error { detail, .. }) => {
                            draft.error = Some(detail);
                            false
                        }
                        Ok(_) => {
                            draft.error = Some(crate::client::OTHER_ANSWER.to_owned());
                            false
                        }
                        Err(err) => {
                            draft.error = Some(crate::client::trouble_words("tiny", &err));
                            false
                        }
                    }
                };
                if reload {
                    this.tiny_emoji_load(cx);
                }
                cx.notify();
            },
        );
    }
}

pub(in crate::gui::tiny) fn emoji(shell: &Shell, facts: &Facts, window: &mut Window, cx: &mut Context<Shell>) -> AnyElement {
    let Some(overlay) = shell.tiny_state.settings.as_ref() else {
        return div().into_any_element();
    };
    let draft = &overlay.emoji;
    let has_stickers = facts.community.has(protocol::TINY_FEATURE_STICKERS);
    let kind = draft.kind_or_default();
    let renaming = draft.renaming.clone();

    let mut form = kit::card()
        .p(px(16.))
        .flex()
        .flex_col()
        .gap(px(12.))
        .child(div().type_small().text_color(theme::text_soft()).child("Name (2 to 32 lowercase letters, numbers, or underscores)"))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(10.))
                .child(div().flex_1().child(kit::input(&shell.tiny_state.settings_fields.emoji_name, None, window, cx)))
                .when(has_stickers && renaming.is_none(), |row| {
                    row.child(
                        segmented()
                            .child(segment("tiny-emoji-kind-emoji", "Emoji", kind == "emoji").on_click(cx.listener(|this, _, _, cx| {
                                if let Some(o) = this.tiny_state.settings.as_mut() {
                                    o.emoji.kind = "emoji";
                                    cx.notify();
                                }
                            })))
                            .child(segment("tiny-emoji-kind-sticker", "Sticker", kind == "sticker").on_click(cx.listener(|this, _, _, cx| {
                                if let Some(o) = this.tiny_state.settings.as_mut() {
                                    o.emoji.kind = "sticker";
                                    cx.notify();
                                }
                            }))),
                    )
                })
                .child(if let Some(id) = renaming.clone() {
                    button("tiny-emoji-save-rename", ButtonKind::Primary, "Save name")
                        .on_click(cx.listener(move |this, _, _, cx| this.tiny_emoji_rename(id.clone(), cx)))
                } else {
                    button_with_icon("tiny-emoji-upload", ButtonKind::Primary, "icons/upload.svg", "Upload…")
                        .on_click(cx.listener(|this, _, _, cx| this.tiny_emoji_upload(cx)))
                }),
        );
    if let Some(detail) = draft.error.as_deref() {
        form = form.child(div().type_small().text_color(theme::danger()).child(detail.to_owned()));
    }

    let mut list = kit::card().child(table_head(&[("Name", None), ("Kind", Some(90.)), ("", Some(160.))]));
    if draft.items.is_empty() {
        list = list.child(div().px(px(14.)).py(px(16.)).type_small().text_color(theme::text_faint()).child(if draft.loading { "Loading…" } else { "No custom emoji yet." }));
    } else {
        for (ix, item) in draft.items.iter().enumerate() {
            let id_rename = item.id.clone();
            let name_rename = item.name.clone();
            let id_delete = item.id.clone();
            let shown_name = if item.kind == "sticker" { item.name.clone() } else { format!(":{}:", item.name) };
            list = list.child(
                table_row(("tiny-emoji-row", ix), false)
                    .child(div().flex_1().min_w_0().truncate().type_ui().text_color(theme::text()).child(shown_name))
                    .child(cell(90., item.kind.clone(), theme::text_soft()))
                    .child(
                        div()
                            .flex_none()
                            .w(px(160.))
                            .flex()
                            .items_center()
                            .justify_end()
                            .gap(px(6.))
                            .child(button(("tiny-emoji-ren", ix), ButtonKind::Secondary, "Rename").on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(o) = this.tiny_state.settings.as_mut() {
                                    o.emoji.renaming = Some(id_rename.clone());
                                }
                                this.tiny_state.settings_fields.emoji_name.update(cx, |i, cx| i.set_text(&name_rename, cx));
                                cx.notify();
                            })))
                            .child(button(("tiny-emoji-del", ix), ButtonKind::Danger, "Delete").on_click(cx.listener(move |this, _, _, cx| this.tiny_emoji_delete(id_delete.clone(), cx)))),
                    ),
            );
        }
    }

    div()
        .w_full()
        .max_w(px(680.))
        .flex()
        .flex_col()
        .gap(px(20.))
        .child(page_head("Emoji & stickers", "Custom emoji and stickers available to every member of this server."))
        .child(form)
        .child(list)
        .into_any_element()
}
