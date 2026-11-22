use super::{ask, button, div, px, theme, ButtonKind, Context, Div, Entity, Key, Network, Request, Response, Shell, Stateful, Type};
use crate::gui::input::TextInput;
use gpui::prelude::*;
use protocol::{Conversation, ConversationKind, DiscordChannelVoice, DiscordVoiceSettings};

pub(in crate::gui) struct Editor {
    target: Key,
    direct: bool,
    settings: Option<DiscordVoiceSettings>,
    bitrate: Entity<TextInput>,
    limit: Entity<TextInput>,
    region: Option<String>,
    quality: u8,
    error: Option<String>,
    saving: bool,
}

pub(in crate::gui) fn discord_voice_editor(row: &Conversation) -> bool {
    row.key.network == Network::Discord && row.kind == ConversationKind::Voice && row.space.is_some() && row.rights.rename
}

fn voice_edit(bitrate: &str, limit: &str, max: u32, region: Option<String>, quality: u8) -> Option<DiscordChannelVoice> {
    let bitrate = bitrate.trim().parse::<u32>().ok()?.checked_mul(1000)?;
    let user_limit = limit.trim().parse::<u32>().ok()?;
    ((8_000..=max).contains(&bitrate) && user_limit <= 99 && matches!(quality, 1 | 2))
        .then_some(DiscordChannelVoice { bitrate, user_limit, rtc_region: region, video_quality_mode: quality })
}

impl Shell {
    pub(in crate::gui) fn open_discord_voice_settings(&mut self, target: Key, direct: bool, cx: &mut Context<Self>) {
        let bitrate = cx.new(|cx| TextInput::new("Bitrate in kb/s", cx));
        let limit = cx.new(|cx| TextInput::new("User limit", cx));
        self.sidebar_state.voice_settings = Some(Editor {
            target: target.clone(), direct, settings: None, bitrate: bitrate.clone(), limit,
            region: None, quality: 1, error: None, saving: false,
        });
        cx.notify();
        let request = Request::DiscordVoiceSettings { conversation: target };
        cx.spawn(async move |shell, cx| {
            let result = cx.background_executor().spawn(async move { ask(&request) }).await;
            let _ = shell.update(cx, |this, cx| {
                let Some(editor) = this.sidebar_state.voice_settings.as_mut().filter(|editor| editor.bitrate == bitrate) else {
                    return;
                };
                match result {
                    Ok(Response::DiscordVoiceSettings { settings }) => {
                        if let Some(channel) = &settings.channel {
                            editor.region = channel.rtc_region.clone();
                            editor.quality = channel.video_quality_mode;
                            editor.bitrate.update(cx, |input, cx| input.set_text(&(channel.bitrate / 1000).to_string(), cx));
                            editor.limit.update(cx, |input, cx| input.set_text(&channel.user_limit.to_string(), cx));
                        } else if direct {
                            editor.region = settings.current_region.clone();
                        }
                        editor.settings = Some(settings);
                    }
                    Ok(Response::Error { detail, .. }) => editor.error = Some(detail),
                    _ => editor.error = Some("Could not read the voice settings.".into()),
                }
                cx.notify();
            });
        }).detach();
    }

    fn save_discord_voice_settings(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.sidebar_state.voice_settings.as_mut().filter(|editor| !editor.saving) else {
            return;
        };
        let Some(settings) = &editor.settings else { return };
        let conversation = editor.target.clone();
        let request = if settings.channel.is_some() {
            let bitrate = editor.bitrate.read(cx).text();
            let limit = editor.limit.read(cx).text();
            let Some(edit) = voice_edit(bitrate, limit, settings.max_bitrate, editor.region.clone(), editor.quality) else {
                editor.error = Some(format!("Use 8–{} kb/s and a limit of 0–99.", settings.max_bitrate / 1000));
                cx.notify();
                return;
            };
            Request::EditDiscordVoiceChannel { conversation, edit }
        } else if editor.direct {
            let Some(region) = editor.region.clone() else { return };
            Request::SetDiscordCallRegion { conversation, region }
        } else {
            return;
        };
        editor.saving = true;
        editor.error = None;
        let form = editor.bitrate.clone();
        cx.notify();
        cx.spawn(async move |shell, cx| {
            let result = cx.background_executor().spawn(async move { ask(&request) }).await;
            let _ = shell.update(cx, |this, cx| {
                let Some(editor) = this.sidebar_state.voice_settings.as_mut().filter(|editor| editor.bitrate == form) else {
                    return;
                };
                editor.saving = false;
                match result {
                    Ok(Response::Ok) => this.sidebar_state.voice_settings = None,
                    Ok(Response::Error { detail, .. }) => editor.error = Some(detail),
                    _ => editor.error = Some("Could not save the voice settings.".into()),
                }
                cx.notify();
            });
        }).detach();
    }

    pub(in crate::gui) fn voice_settings_modal(&self, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        let editor = self.sidebar_state.voice_settings.as_ref()?;
        let mut card = div().p(px(16.)).w(px(410.)).max_w_full().bg(theme::overlay()).rounded(px(theme::R_FLOAT))
            .flex().flex_col().gap(px(8.))
            .child(div().type_title().child(if editor.direct { "Change region" } else { "Voice settings" }));
        let mut editable = false;
        if let Some(settings) = &editor.settings {
            if let Some(region) = &settings.current_region {
                let name = settings.regions.iter().find(|option| &option.id == region).map_or(region, |option| &option.name);
                card = card.child(div().type_small().text_color(theme::text_soft()).child(format!("Connected region · {name}")));
            }
            editable = settings.channel.is_some() || (editor.direct && !settings.regions.is_empty());
            if settings.channel.is_some() {
                let field = |label: String, input| div().flex().flex_col().gap(px(4.))
                    .child(div().type_small().text_color(theme::text_soft()).child(label))
                    .child(div().p(px(8.)).bg(theme::canvas()).child(input));
                card = card
                    .child(field(format!("Bitrate (8–{} kb/s)", settings.max_bitrate / 1000), editor.bitrate.clone()))
                    .child(field("User limit (0 = unlimited)".into(), editor.limit.clone()))
                    .child(div().type_small().child("Video quality"))
                    .child(div().flex().gap(px(6.)).children([(1, "Auto"), (2, "720p")].map(|(quality, label)| {
                        button(("discord-quality", quality as usize), if editor.quality == quality { ButtonKind::Primary } else { ButtonKind::Secondary }, label)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(editor) = this.sidebar_state.voice_settings.as_mut() {
                                    editor.quality = quality;
                                    cx.notify();
                                }
                            }))
                    })));
            }
            if editable {
                let mut options = Vec::new();
                if !editor.direct {
                    options.push((None, "Automatic".to_owned()));
                }
                options.extend(settings.regions.iter().map(|region| (Some(region.id.clone()), region.name.clone())));
                if let Some(current) = editor.region.as_ref().filter(|id| !options.iter().any(|(option, _)| option.as_ref() == Some(id))) {
                    options.push((Some(current.clone()), current.clone()));
                }
                let choices: Vec<_> = options.into_iter().enumerate().map(|(ix, (region, name))| {
                    button(("discord-voice-region", ix), if editor.region == region { ButtonKind::Primary } else { ButtonKind::Secondary }, name)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(editor) = this.sidebar_state.voice_settings.as_mut() {
                                editor.region = region.clone();
                                cx.notify();
                            }
                        }))
                }).collect();
                card = card.child(div().type_small().child("Region"))
                    .child(div().id("discord-region-list").max_h(px(160.)).overflow_y_scroll().flex().flex_col().gap(px(4.)).children(choices));
            } else if settings.current_region.is_none() {
                card = card.child(div().type_small().text_color(theme::text_soft()).child("Waiting for the voice connection."));
            }
        } else if editor.error.is_none() {
            card = card.child(div().type_small().text_color(theme::text_soft()).child("Loading…"));
        }
        let enabled = editable && !editor.saving && (!editor.direct || editor.region.is_some());
        card = card.children(editor.error.iter().map(|error| div().type_small().text_color(theme::danger()).child(error.clone())))
            .child(div().flex().justify_end().gap(px(8.))
                .child(button("voice-settings-close", ButtonKind::Ghost, if editable { "Cancel" } else { "Close" })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.sidebar_state.voice_settings = None;
                        cx.notify();
                    })))
                .children(editable.then(|| button("voice-settings-save", if enabled { ButtonKind::Primary } else { ButtonKind::Ghost }, if editor.saving { "Saving…" } else { "Save" })
                    .when(enabled, |button| button.on_click(cx.listener(|this, _, _, cx| this.save_discord_voice_settings(cx)))))));
        Some(div().id("voice-settings-backdrop").absolute().inset_0().bg(theme::scrim()).flex().items_center().justify_center().child(card))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::{AccountKey, Key};

    #[test]
    fn an_unmanaged_channel_has_no_voice_editor_and_edits_obey_the_tier_limit() {
        let key = Key::new(&AccountKey { network: Network::Discord, account: "me".into() }, "voice");
        let mut voice = Conversation::new(key, ConversationKind::Voice, "Lounge");
        voice.space = Some("guild".into());
        assert!(!discord_voice_editor(&voice));
        voice.rights.rename = true;
        assert!(discord_voice_editor(&voice));
        assert!(voice_edit("128", "99", 96_000, None, 2).is_none());
        assert!(voice_edit("32", "100", 96_000, None, 2).is_none());
        assert_eq!(voice_edit("32", "0", 96_000, None, 2).unwrap().bitrate, 32_000);
    }
}
