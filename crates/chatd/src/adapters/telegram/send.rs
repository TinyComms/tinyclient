//! Sending with more than text: files uploaded in 512 KB parts (`upload.saveFilePart`, or
//! `saveBigFilePart` past 10 MB) through the one upload helper, sent as one message or as
//! albums of up to ten; a sticker or GIF from the picker, which uploads nothing; and Telegram's
//! own silent and scheduled sends, which Telegram carries out. tinyclient runs no timer.

use super::catalog::{self, Pick};
use super::peers::Who;
use super::wire::Fault;
use super::{live, random_id, remember_random, reply_header, Account, Options};
use crate::blobs::upload::{self, Upload};
use grammers_tl_types as tl;
use protocol::{Message, Outgoing, VoiceNote};
use std::sync::Arc;

/// Telegram's part size, which every part but the last must be exactly.
const PART: usize = 512 * 1024;
/// Past this a file is a "big" file, uploaded with `saveBigFilePart`.
const BIG: u64 = 10 * 1024 * 1024;
/// Most items in one album.
const ALBUM: usize = 10;
/// Pictures Telegram takes as photos (compressed); anything else goes as a document.
const PHOTOS: &[&str] = &["image/jpeg", "image/png", "image/webp"];

/// The send options Telegram carries out.
fn options(with: &Outgoing) -> Options {
    Options {
        silent: with.silent,
        no_webpage: with.suppress_embeds,
        schedule: with.schedule.and_then(|at| i32::try_from(at).ok()),
    }
}

pub(super) fn send_with(account: &Arc<Account>, conversation: &str, client_id: &str, text: &str, reply_to: Option<&str>, with: Outgoing) {
    let ctx = account.ctx.clone();
    let opts = options(&with);
    account.here();
    if super::secret::is_row(conversation) {
        let fault = Fault::Rpc { code: 400, name: "SECRET_CHAT".to_owned(), value: None };
        ctx.sent(conversation, client_id, Err(fault.words()));
        return;
    }
    if with.files.is_empty() {
        let outcome = if let Some(contact) = &with.contact {
            send_contact(account, conversation, client_id, text, reply_to, contact, opts)
        } else {
            match &with.pick {
                Some(pick) => send_pick(account, conversation, client_id, text, reply_to, &pick.id, opts),
                None => match pasted_place(text) {
                    Some(found) => send_place(account, conversation, client_id, reply_to, &found, opts),
                    None => match dice_emoticon(text) {
                        Some(emoticon) => send_dice(account, conversation, client_id, text, reply_to, emoticon, opts),
                        None => super::send_text(account, conversation, client_id, text, reply_to, opts, &with.spans),
                    },
                },
            }
        };
        ctx.sent(conversation, client_id, outcome.map_err(|fault| fault.words()));
        return;
    }
    let (account, conversation_id, client, text, reply) =
        (account.clone(), conversation.to_owned(), client_id.to_owned(), text.to_owned(), reply_to.map(str::to_owned));
    let voice: Option<VoiceNote> = (with.files.len() == 1).then_some(()).and_then(|_| with.voice.clone());
    upload::spawn(&ctx, conversation, client_id, &with.files, move |up| {
        files(&account, &conversation_id, &client, &text, reply.as_deref(), opts, voice.as_ref(), up)
    });
}

pub(super) fn cancel(account: &Account, conversation: &str, client_id: &str) {
    upload::cancel(&account.ctx.account, client_id);
    super::deliver_cancel(account, conversation);
}

/// Where a send goes and what it answers.
struct Target {
    who: Who,
    peer: tl::enums::InputPeer,
    reply_to: Option<tl::enums::InputReplyTo>,
}

fn target(account: &Account, conversation: &str, reply_to: Option<&str>) -> Result<Target, Fault> {
    let (who, topic) = Who::parse(conversation).ok_or_else(|| Fault::Rpc { code: 400, name: "PEER_ID_INVALID".to_owned(), value: None })?;
    let reply = reply_to.and_then(|id| id.parse::<i32>().ok());
    // The comments of a channel post are held in the discussion group behind it: like a
    // forum's send names its topic, theirs names the echo of the post there.
    let (who, topic) = super::sync::comment_target(account, conversation)?.map(|(group, echo)| (group, Some(echo))).unwrap_or((who, topic));
    Ok(Target { who, peer: account.input(who)?, reply_to: reply_header(reply, topic) })
}

/// The message Telegram made of a send, or of a scheduled one, from its answer.
fn made(account: &Account, conversation: &str, target: &Target, text: &str, opts: Options, answer: &tl::enums::Updates) -> Result<Message, Fault> {
    live::answered_scheduled(account, conversation, text, opts, answer)
        .or_else(|| live::sent(account, conversation, target.who, text, None, answer))
        .ok_or_else(|| Fault::Net("Telegram accepted the message but did not name it".to_owned()))
}

fn send_pick(account: &Account, conversation: &str, client_id: &str, text: &str, reply_to: Option<&str>, pick: &str, opts: Options) -> Result<Message, Fault> {
    let target = target(account, conversation, reply_to)?;
    let random = random_id(client_id);
    let (plain, entities) = super::prepared(account, conversation, text);
    let (answer, caption) = match catalog::find(account, pick).ok_or_else(|| Fault::Rpc { code: 400, name: "PICK_EXPIRED".to_owned(), value: None })? {
        Pick::Document(id) => {
            remember_random(account, random, client_id);
            let media = tl::types::InputMediaDocument { spoiler: false, id: id.into(), video_cover: None, video_timestamp: None, ttl_seconds: None, query: None };
            let answer = account.ask(&send_media(&target, media.into(), &plain, entities, random, opts))?;
            (answer, plain.as_str())
        }
        Pick::Inline { query_id, result } => {
            // A bot's result carries no caption: words typed with it go first, on their own.
            if !text.trim().is_empty() {
                super::send_text(account, conversation, &format!("{client_id}1"), text, None, opts, &[])?;
            }
            remember_random(account, random, client_id);
            let sent = account.ask(&tl::functions::messages::SendInlineBotResult {
                silent: opts.silent,
                background: false,
                clear_draft: true,
                hide_via: false,
                peer: target.peer.clone(),
                reply_to: target.reply_to.clone(),
                random_id: random,
                query_id,
                id: result,
                schedule_date: opts.schedule,
                send_as: None,
                quick_reply_shortcut: None,
                allow_paid_stars: None,
            })?;
            (sent, "")
        }
    };
    made(account, conversation, &target, caption, opts, &answer)
}

/// A message that is only a place the person pasted. Words around the numbers stay text.
pub(super) enum PastedPlace {
    Point { lat: f64, long: f64 },
    Venue { title: String, address: String, lat: f64, long: f64 },
}

/// `lat, lon` or `geo:lat,lon`. Out of range, or anything else on the line, is not a place.
fn coordinate_pair(text: &str) -> Option<(f64, f64)> {
    let text = text.trim();
    let text = text.strip_prefix("geo:").unwrap_or(text).trim();
    let (lat, long) = text.split_once(',')?;
    let lat: f64 = lat.trim().parse().ok()?;
    let long: f64 = long.trim().parse().ok()?;
    if !lat.is_finite() || !long.is_finite() || !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&long) {
        return None;
    }
    Some((lat, long))
}

pub(super) fn pasted_place(text: &str) -> Option<PastedPlace> {
    let text = text.trim();
    if let Some(rest) = text.strip_prefix("venue:").or_else(|| text.strip_prefix("Venue:")) {
        let mut parts = rest.split(" | ");
        let title = parts.next()?.trim();
        let address = parts.next()?.trim();
        let coords = parts.next()?.trim();
        if parts.next().is_some() || title.is_empty() {
            return None;
        }
        let (lat, long) = coordinate_pair(coords)?;
        return Some(PastedPlace::Venue { title: title.to_owned(), address: address.to_owned(), lat, long });
    }
    let (lat, long) = coordinate_pair(text)?;
    Some(PastedPlace::Point { lat, long })
}

fn input_point(lat: f64, long: f64) -> tl::enums::InputGeoPoint {
    tl::types::InputGeoPoint { lat, long, accuracy_radius: None }.into()
}

pub(super) fn pasted_media(place: &PastedPlace) -> tl::enums::InputMedia {
    match place {
        PastedPlace::Point { lat, long } => tl::types::InputMediaGeoPoint { geo_point: input_point(*lat, *long) }.into(),
        PastedPlace::Venue { title, address, lat, long } => tl::types::InputMediaVenue {
            geo_point: input_point(*lat, *long),
            title: title.clone(),
            address: address.clone(),
            provider: String::new(),
            venue_id: String::new(),
            venue_type: String::new(),
        }
        .into(),
    }
}

/// A pasted place, sent as the location itself. The numbers are not a caption.
fn send_place(account: &Account, conversation: &str, client_id: &str, reply_to: Option<&str>, place: &PastedPlace, opts: Options) -> Result<Message, Fault> {
    let target = target(account, conversation, reply_to)?;
    let random = random_id(client_id);
    remember_random(account, random, client_id);
    let answer = account.ask(&send_media(&target, pasted_media(place), "", None, random, opts))?;
    made(account, conversation, &target, "", opts, &answer)
}

/// A message that is only one of Telegram's dice: 🎲 🎯 🏀 ⚽ 🎳 🎰. A variation selector
/// does not change which. Anything else, including the emoji plus words, is ordinary text.
pub(super) fn dice_emoticon(text: &str) -> Option<&'static str> {
    const DICE: &[&str] = &["\u{1F3B2}", "\u{1F3AF}", "\u{1F3C0}", "\u{26BD}", "\u{1F3B3}", "\u{1F3B0}"];
    let bare: String = text.trim().chars().filter(|c| *c != '\u{FE0F}').collect();
    DICE.iter().copied().find(|emoji| *emoji == bare)
}

/// A lone dice emoji, sent as the roll rather than as text. The caption stays empty: the
/// emoji is the roll, and the value comes back on the message Telegram makes.
fn send_dice(account: &Account, conversation: &str, client_id: &str, text: &str, reply_to: Option<&str>, emoticon: &str, opts: Options) -> Result<Message, Fault> {
    let target = target(account, conversation, reply_to)?;
    let random = random_id(client_id);
    remember_random(account, random, client_id);
    let media = tl::types::InputMediaDice { emoticon: emoticon.to_owned() }.into();
    let answer = account.ask(&send_media(&target, media, "", None, random, opts))?;
    made(account, conversation, &target, text, opts, &answer)
}

/// Shares a contact card via `messages.sendMedia` (`InputMediaContact`).
fn send_contact(
    account: &Account,
    conversation: &str,
    client_id: &str,
    text: &str,
    reply_to: Option<&str>,
    contact: &protocol::SharedContact,
    opts: Options,
) -> Result<Message, Fault> {
    let target = target(account, conversation, reply_to)?;
    let random = random_id(client_id);
    remember_random(account, random, client_id);
    let (first_name, last_name) = super::split_name(contact.name.trim());
    let phone_number = contact.phones.first().cloned().unwrap_or_default();
    let mut vcard = String::new();
    if !contact.emails.is_empty() || !contact.organization.trim().is_empty() {
        vcard.push_str("BEGIN:VCARD\nVERSION:3.0\n");
        if !contact.name.trim().is_empty() {
            vcard.push_str(&format!("FN:{}\n", contact.name.trim()));
        }
        if !contact.organization.trim().is_empty() {
            vcard.push_str(&format!("ORG:{}\n", contact.organization.trim()));
        }
        for email in &contact.emails {
            let e = email.trim();
            if !e.is_empty() {
                vcard.push_str(&format!("EMAIL:{e}\n"));
            }
        }
        vcard.push_str("END:VCARD");
    }
    let media: tl::enums::InputMedia = tl::types::InputMediaContact {
        phone_number,
        first_name,
        last_name,
        vcard,
    }
    .into();
    let (plain, entities) = super::prepared(account, conversation, text);
    let answer = account.ask(&send_media(&target, media, &plain, entities, random, opts))?;
    made(account, conversation, &target, &plain, opts, &answer)
}

fn send_media(
    target: &Target,
    media: tl::enums::InputMedia,
    caption: &str,
    entities: Option<Vec<tl::enums::MessageEntity>>,
    random_id: i64,
    opts: Options,
) -> tl::functions::messages::SendMedia {
    tl::functions::messages::SendMedia {
        silent: opts.silent,
        background: false,
        clear_draft: true,
        noforwards: false,
        update_stickersets_order: false,
        invert_media: false,
        allow_paid_floodskip: false,
        peer: target.peer.clone(),
        reply_to: target.reply_to.clone(),
        media,
        message: caption.to_owned(),
        random_id,
        reply_markup: None,
        entities,
        schedule_date: opts.schedule,
        schedule_repeat_period: None,
        send_as: None,
        quick_reply_shortcut: None,
        effect: None,
        allow_paid_stars: None,
        suggested_post: None,
    }
}

/// One uploaded file, as what a message names.
struct Uploaded {
    media: tl::enums::InputMedia,
    /// A photo or a video: these share an album; documents make their own.
    visual: bool,
}

fn files(account: &Account, conversation: &str, client_id: &str, text: &str, reply_to: Option<&str>, opts: Options, voice: Option<&VoiceNote>, up: &mut Upload) -> Result<Message, String> {
    let words = |fault: Fault| fault.words();
    let target = target(account, conversation, reply_to).map_err(words)?;
    let premium = account.dir().get(Who::User(account.me)).is_some_and(|known| known.premium);
    let limit = account.live().limits.caption(premium) as usize;
    let (plain, entities) = super::prepared(account, conversation, text);
    // A caption longer than Telegram takes goes first as a message of its own.
    let caption = if plain.chars().count() > limit {
        super::send_text(account, conversation, &format!("{client_id}1"), text, reply_to, opts, &[]).map_err(words)?;
        ""
    } else {
        plain.as_str()
    };
    let entities = (!caption.is_empty()).then_some(entities).flatten();
    let mut uploaded = Vec::with_capacity(up.files().len());
    for at in 0..up.files().len() {
        // `voice` is only set for a send of one file: a voice message is one file.
        uploaded.push(file(account, conversation, client_id, at, voice, up)?);
    }
    up.check()?;
    let random = random_id(client_id);
    remember_random(account, random, client_id);
    if uploaded.len() == 1 {
        let Some(one) = uploaded.pop() else {
            return Err("Nothing was sent.".to_owned());
        };
        let answer = account.ask(&send_media(&target, one.media, caption, entities.clone(), random, opts)).map_err(words)?;
        return made(account, conversation, &target, caption, opts, &answer).map_err(words);
    }
    // Albums: pictures and videos together, documents together, ten at most each. The caption
    // goes under the first item, as Telegram's apps put it.
    let (visual, documents): (Vec<Uploaded>, Vec<Uploaded>) = uploaded.into_iter().partition(|one| one.visual);
    let mut first = None;
    let mut index = 0u64;
    for group in [visual, documents] {
        for album in group.chunks(ALBUM) {
            // One left over is a message of its own: an album holds two or more.
            if let [one] = album {
                let item_random = if index == 0 { random } else { random_id(&format!("{client_id}:{index}")) };
                let shown = if index == 0 { caption } else { "" };
                let caption_entities = (index == 0).then(|| entities.clone()).flatten();
                index += 1;
                let answer = account.ask(&send_media(&target, one.media.clone(), shown, caption_entities, item_random, opts)).map_err(words)?;
                match first {
                    None => first = Some(made(account, conversation, &target, shown, opts, &answer).map_err(words)?),
                    Some(_) => live::answered(account, &answer),
                }
                continue;
            }
            let mut items = Vec::with_capacity(album.len());
            for one in album {
                // An album names media Telegram already holds: each upload is made one first.
                let held = account
                    .ask(&tl::functions::messages::UploadMedia { business_connection_id: None, peer: target.peer.clone(), media: one.media.clone() })
                    .map_err(words)?;
                let media = stored(held, &one.media).ok_or_else(|| "Telegram did not keep the file for the album.".to_owned())?;
                let item_random = if index == 0 { random } else { random_id(&format!("{client_id}:{index}")) };
                items.push(tl::enums::InputSingleMedia::Media(tl::types::InputSingleMedia {
                    media,
                    random_id: item_random,
                    message: if index == 0 { caption.to_owned() } else { String::new() },
                    entities: (index == 0).then(|| entities.clone()).flatten(),
                }));
                index += 1;
                up.check()?;
            }
            let answer = account
                .ask(&tl::functions::messages::SendMultiMedia {
                    silent: opts.silent,
                    background: false,
                    clear_draft: true,
                    noforwards: false,
                    update_stickersets_order: false,
                    invert_media: false,
                    allow_paid_floodskip: false,
                    peer: target.peer.clone(),
                    reply_to: target.reply_to.clone(),
                    multi_media: items,
                    schedule_date: opts.schedule,
                    send_as: None,
                    quick_reply_shortcut: None,
                    effect: None,
                    allow_paid_stars: None,
                })
                .map_err(words)?;
            if first.is_none() {
                first = Some(made(account, conversation, &target, caption, opts, &answer).map_err(words)?);
            } else {
                live::answered(account, &answer);
            }
        }
    }
    first.ok_or_else(|| "Nothing was sent.".to_owned())
}

/// Uploads one replacement file for `messages.editMessage` (`media`), reusing the same
/// part-streaming, photo/document detection, spoiler, and video/audio probing as sends.
pub(super) fn upload_single(
    account: &Account,
    conversation: &str,
    key_seed: &str,
    source: &protocol::OutgoingFile,
) -> Result<tl::enums::InputMedia, String> {
    let mut up = Upload::with_progress(&account.ctx.account, key_seed, std::slice::from_ref(source), |_, _| {})?;
    let uploaded = file(account, conversation, key_seed, 0, None, &mut up);
    up.done();
    uploaded.map(|u| u.media)
}

/// One file of a send, in parts, as Telegram's `InputMedia`. A voice send carries its facts in
/// `voice` and is sent as a voice note, as Telegram's apps send one.
fn file(account: &Account, conversation: &str, client_id: &str, at: usize, voice: Option<&VoiceNote>, up: &mut Upload) -> Result<Uploaded, String> {
    let (name, bytes, mime, spoiler, as_file, path, staged_media, thumb_path) = {
        let source = &up.files()[at];
        (
            source.name.clone(),
            source.bytes,
            source.mime,
            source.spoiler,
            source.as_file,
            source.path.clone(),
            source.media.clone(),
            source.thumb.clone(),
        )
    };
    let photo = !as_file && PHOTOS.contains(&mime) && bytes <= BIG;
    let is_video = !as_file && voice.is_none() && mime.starts_with("video/");
    let is_audio = !as_file && voice.is_none() && mime.starts_with("audio/");
    let activity = if voice.is_some() {
        protocol::Compose::UploadingAudio
    } else if photo {
        protocol::Compose::UploadingPhoto
    } else if is_video {
        protocol::Compose::UploadingVideo
    } else {
        protocol::Compose::UploadingFile
    };
    let file_id = random_id(&format!("{client_id}/{at}/{name}"));
    let big = bytes > BIG;
    let mut parts = 1;
    let mut told = -1i32;
    up.parts(at, PART, 0, |part| {
        parts = part.count as i32;
        let progress = if part.count == 0 { 0 } else { (((part.index as u64 + 1) * 100) / part.count as u64).min(100) as i32 };
        if progress != told {
            told = progress;
            super::show_upload(account, conversation, activity, progress);
        }
        let saved = match big {
            true => account.ask(&tl::functions::upload::SaveBigFilePart {
                file_id,
                file_part: part.index as i32,
                file_total_parts: part.count as i32,
                bytes: part.bytes.to_vec(),
            }),
            false => account.ask(&tl::functions::upload::SaveFilePart { file_id, file_part: part.index as i32, bytes: part.bytes.to_vec() }),
        };
        match saved {
            Ok(true) => Ok(()),
            Ok(false) => Err(format!("{name} could not be sent: Telegram did not keep a part of it.")),
            Err(fault) => Err(format!("{name} could not be sent: {}", fault.words())),
        }
    })?;
    let input: tl::enums::InputFile = match big {
        true => tl::types::InputFileBig { id: file_id, parts, name: name.clone() }.into(),
        // The checksum is optional; Telegram checks the parts itself.
        false => tl::types::InputFile { id: file_id, parts, name: name.clone(), md5_checksum: String::new() }.into(),
    };
    let thumb = match thumb_path.as_deref() {
        Some(thumb_file) if is_video => upload_thumb(account, client_id, at, thumb_file)?,
        _ => None,
    };
    // A voice note is one Ogg Opus file and its own attribute: no file name, which Telegram's
    // own voice notes carry none of (their rows read "Voice message"). The loudness bars go
    // from the contract's byte a bar to Telegram's five bits a bar.
    let attributes: Vec<tl::enums::DocumentAttribute> = match voice {
        Some(voice) => vec![tl::types::DocumentAttributeAudio {
            voice: true,
            duration: (voice.duration / 1000) as i32,
            title: None,
            performer: None,
            waveform: Some(packed(&voice.waveform)),
        }
        .into()],
        None => {
            let mut attrs: Vec<tl::enums::DocumentAttribute> = vec![tl::types::DocumentAttributeFilename { file_name: name }.into()];
            if is_video {
                let probed = probe_mp4(&path);
                let duration = staged_media
                    .as_ref()
                    .and_then(|m| m.duration_ms)
                    .map(|ms| f64::from(ms) / 1000.0)
                    .or_else(|| probed.as_ref().map(|p| p.duration))
                    .unwrap_or(0.0);
                let w = staged_media
                    .as_ref()
                    .and_then(|m| m.width)
                    .or_else(|| probed.as_ref().map(|p| p.width))
                    .unwrap_or(0) as i32;
                let h = staged_media
                    .as_ref()
                    .and_then(|m| m.height)
                    .or_else(|| probed.as_ref().map(|p| p.height))
                    .unwrap_or(0) as i32;
                let supports_streaming = probed.as_ref().is_some_and(|p| p.moov_first);
                attrs.push(
                    tl::types::DocumentAttributeVideo {
                        round_message: false,
                        supports_streaming,
                        nosound: false,
                        duration,
                        w,
                        h,
                        preload_prefix_size: None,
                        video_start_ts: None,
                        video_codec: None,
                    }
                    .into(),
                );
            } else if is_audio {
                let probed = probe_audio_tags(&path);
                let duration = staged_media.as_ref().and_then(|m| m.duration_ms).map(|ms| (ms / 1000) as i32).unwrap_or(0);
                let title = staged_media.as_ref().and_then(|m| m.title.clone()).or(probed.title);
                let performer = staged_media.as_ref().and_then(|m| m.performer.clone()).or(probed.performer);
                if duration > 0 || title.is_some() || performer.is_some() {
                    attrs.push(
                        tl::types::DocumentAttributeAudio {
                            voice: false,
                            duration,
                            title,
                            performer,
                            waveform: None,
                        }
                        .into(),
                    );
                }
            }
            attrs
        }
    };
    let media: tl::enums::InputMedia = match photo {
        true => tl::types::InputMediaUploadedPhoto { spoiler, live_photo: false, file: input, stickers: None, ttl_seconds: None, video: None }.into(),
        false => tl::types::InputMediaUploadedDocument {
            nosound_video: false,
            force_file: as_file,
            spoiler: spoiler && !as_file,
            file: input,
            thumb,
            mime_type: mime.to_owned(),
            attributes,
            stickers: None,
            video_cover: None,
            video_timestamp: None,
            ttl_seconds: None,
        }
        .into(),
    };
    Ok(Uploaded { media, visual: photo || is_video })
}

fn upload_thumb(account: &Account, client_id: &str, at: usize, path: &std::path::Path) -> Result<Option<tl::enums::InputFile>, String> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) if !bytes.is_empty() && bytes.len() <= 2 * 1024 * 1024 => bytes,
        _ => return Ok(None),
    };
    let file_id = random_id(&format!("{client_id}/{at}/thumb"));
    let chunks: Vec<&[u8]> = bytes.chunks(PART).collect();
    let parts = chunks.len() as i32;
    for (index, chunk) in chunks.into_iter().enumerate() {
        let saved = account.ask(&tl::functions::upload::SaveFilePart {
            file_id,
            file_part: index as i32,
            bytes: chunk.to_vec(),
        });
        match saved {
            Ok(true) => {}
            Ok(false) => return Err("Thumbnail could not be sent: Telegram did not keep a part of it.".to_owned()),
            Err(fault) => return Err(format!("Thumbnail could not be sent: {}", fault.words())),
        }
    }
    Ok(Some(tl::types::InputFile { id: file_id, parts, name: "thumb.jpg".to_owned(), md5_checksum: String::new() }.into()))
}

#[derive(Debug, Default, PartialEq)]
struct Mp4Info {
    width: u32,
    height: u32,
    duration: f64,
    moov_first: bool,
}

/// Inspects an ISO BMFF (`.mp4` / `.mov` / `.m4v`) file for `moov` vs `mdat` ordering,
/// duration (`mvhd`), and display dimensions (`trak` -> `tkhd`).
fn probe_mp4(path: &std::path::Path) -> Option<Mp4Info> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let mut pos = 0u64;
    let mut seen_mdat = false;
    let mut info = Mp4Info::default();
    let mut found_moov = false;
    while pos + 8 <= len {
        file.seek(SeekFrom::Start(pos)).ok()?;
        let mut header = [0u8; 8];
        file.read_exact(&mut header).ok()?;
        let size32 = u32::from_be_bytes([header[0], header[1], header[2], header[3]]);
        let kind = &header[4..8];
        let (box_size, header_len) = match size32 {
            0 => (len.saturating_sub(pos), 8u64),
            1 => {
                if pos + 16 > len {
                    break;
                }
                let mut ext = [0u8; 8];
                file.read_exact(&mut ext).ok()?;
                (u64::from_be_bytes(ext), 16u64)
            }
            n => (u64::from(n), 8u64),
        };
        if box_size < header_len || pos + box_size > len {
            break;
        }
        if kind == b"mdat" {
            seen_mdat = true;
        } else if kind == b"moov" {
            found_moov = true;
            info.moov_first = !seen_mdat;
            let payload_len = (box_size - header_len).min(8 * 1024 * 1024) as usize;
            let mut moov = vec![0u8; payload_len];
            if file.read_exact(&mut moov).is_ok() {
                parse_moov(&moov, &mut info);
            }
            break;
        }
        pos += box_size;
    }
    found_moov.then_some(info)
}

fn parse_moov(buf: &[u8], info: &mut Mp4Info) {
    let mut at = 0usize;
    while at + 8 <= buf.len() {
        let size = u32::from_be_bytes([buf[at], buf[at + 1], buf[at + 2], buf[at + 3]]) as usize;
        let kind = &buf[at + 4..at + 8];
        let box_end = if size == 0 { buf.len() } else { at.saturating_add(size) };
        if size > 0 && size < 8 || box_end > buf.len() {
            break;
        }
        let body = &buf[at + 8..box_end];
        if kind == b"mvhd" && body.len() >= 20 {
            let version = body[0];
            if version == 0 && body.len() >= 20 {
                let timescale = u32::from_be_bytes([body[12], body[13], body[14], body[15]]);
                let duration = u32::from_be_bytes([body[16], body[17], body[18], body[19]]);
                if timescale > 0 {
                    info.duration = f64::from(duration) / f64::from(timescale);
                }
            } else if version == 1 && body.len() >= 32 {
                let timescale = u32::from_be_bytes([body[20], body[21], body[22], body[23]]);
                let duration = u64::from_be_bytes([body[24], body[25], body[26], body[27], body[28], body[29], body[30], body[31]]);
                if timescale > 0 {
                    info.duration = duration as f64 / f64::from(timescale);
                }
            }
        } else if kind == b"trak" && (info.width == 0 || info.height == 0) {
            parse_trak(body, info);
        }
        at = box_end;
    }
}

fn parse_trak(buf: &[u8], info: &mut Mp4Info) {
    let mut at = 0usize;
    while at + 8 <= buf.len() {
        let size = u32::from_be_bytes([buf[at], buf[at + 1], buf[at + 2], buf[at + 3]]) as usize;
        let kind = &buf[at + 4..at + 8];
        let box_end = if size == 0 { buf.len() } else { at.saturating_add(size) };
        if size > 0 && size < 8 || box_end > buf.len() {
            break;
        }
        if kind == b"tkhd" {
            let body = &buf[at + 8..box_end];
            if let Some(&version) = body.first() {
                let dims_at = if version == 1 { 88 } else { 76 };
                if body.len() >= dims_at + 8 {
                    let w = u32::from_be_bytes([body[dims_at], body[dims_at + 1], body[dims_at + 2], body[dims_at + 3]]) >> 16;
                    let h = u32::from_be_bytes([body[dims_at + 4], body[dims_at + 5], body[dims_at + 6], body[dims_at + 7]]) >> 16;
                    if w > 0 && h > 0 {
                        info.width = w;
                        info.height = h;
                        return;
                    }
                }
            }
        }
        at = box_end;
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
struct AudioTags {
    title: Option<String>,
    performer: Option<String>,
}

/// Reads ID3v2 (`TIT2`/`TPE1`) or trailing ID3v1 tags for music track title and performer.
fn probe_audio_tags(path: &std::path::Path) -> AudioTags {
    use std::io::{Read, Seek, SeekFrom};
    let mut tags = AudioTags::default();
    let Ok(mut file) = std::fs::File::open(path) else {
        return tags;
    };
    let mut head = [0u8; 10];
    if file.read_exact(&mut head).is_ok() && &head[0..3] == b"ID3" && matches!(head[3], 3 | 4) {
        let size = ((usize::from(head[6] & 0x7f)) << 21)
            | ((usize::from(head[7] & 0x7f)) << 14)
            | ((usize::from(head[8] & 0x7f)) << 7)
            | usize::from(head[9] & 0x7f);
        let mut buf = vec![0u8; size.min(256 * 1024)];
        if file.read_exact(&mut buf).is_ok() {
            let mut at = 0usize;
            while at + 10 <= buf.len() {
                let frame_id = &buf[at..at + 4];
                if frame_id[0] == 0 {
                    break;
                }
                let frame_len = if head[3] == 4 {
                    ((usize::from(buf[at + 4] & 0x7f)) << 21)
                        | ((usize::from(buf[at + 5] & 0x7f)) << 14)
                        | ((usize::from(buf[at + 6] & 0x7f)) << 7)
                        | usize::from(buf[at + 7] & 0x7f)
                } else {
                    u32::from_be_bytes([buf[at + 4], buf[at + 5], buf[at + 6], buf[at + 7]]) as usize
                };
                at += 10;
                if frame_len == 0 || at + frame_len > buf.len() {
                    break;
                }
                let data = &buf[at..at + frame_len];
                if matches!(frame_id, b"TIT2" | b"TPE1") && !data.is_empty() {
                    let text = match data[0] {
                        0 | 3 => String::from_utf8_lossy(&data[1..]).trim_matches('\0').trim().to_owned(),
                        _ => String::new(),
                    };
                    if !text.is_empty() {
                        if frame_id == b"TIT2" {
                            tags.title = Some(text);
                        } else {
                            tags.performer = Some(text);
                        }
                    }
                }
                at += frame_len;
            }
        }
    }
    if tags.title.is_none() && tags.performer.is_none() {
        if let Ok(meta) = file.metadata() {
            if meta.len() >= 128 && file.seek(SeekFrom::End(-128)).is_ok() {
                let mut tail = [0u8; 128];
                if file.read_exact(&mut tail).is_ok() && &tail[0..3] == b"TAG" {
                    let clean = |slice: &[u8]| {
                        let s = String::from_utf8_lossy(slice).trim_matches('\0').trim().to_owned();
                        (!s.is_empty()).then_some(s)
                    };
                    tags.title = clean(&tail[3..33]);
                    tags.performer = clean(&tail[33..63]);
                }
            }
        }
    }
    tags
}

/// A voice message's loudness, one byte a bar, as Telegram packs it: five-bit values end to
/// end, as [`content::waveform`](super::content::waveform) unpacks them.
fn packed(bars: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bars.len() * 5 / 8 + 1);
    let (mut bits, mut held) = (0usize, 0u16);
    for &bar in bars {
        held |= u16::from(u16::from(bar) * 31 / 255) << bits;
        bits += 5;
        while bits >= 8 {
            out.push((held & 0xff) as u8);
            held >>= 8;
            bits -= 8;
        }
    }
    if bits > 0 {
        out.push((held & 0xff) as u8);
    }
    out
}

/// What `messages.uploadMedia` kept, as media an album can name, with the spoiler kept.
fn stored(held: tl::enums::MessageMedia, asked: &tl::enums::InputMedia) -> Option<tl::enums::InputMedia> {    let spoiler = match asked {
        tl::enums::InputMedia::UploadedPhoto(photo) => photo.spoiler,
        tl::enums::InputMedia::UploadedDocument(document) => document.spoiler,
        _ => false,
    };
    match held {
        tl::enums::MessageMedia::Photo(media) => match media.photo? {
            tl::enums::Photo::Photo(photo) => Some(
                tl::types::InputMediaPhoto {
                    spoiler,
                    live_photo: false,
                    id: tl::types::InputPhoto { id: photo.id, access_hash: photo.access_hash, file_reference: photo.file_reference }.into(),
                    ttl_seconds: None,
                    video: None,
                }
                .into(),
            ),
            tl::enums::Photo::Empty(_) => None,
        },
        tl::enums::MessageMedia::Document(media) => match media.document? {
            tl::enums::Document::Document(document) => Some(
                tl::types::InputMediaDocument {
                    spoiler,
                    id: tl::types::InputDocument { id: document.id, access_hash: document.access_hash, file_reference: document.file_reference }.into(),
                    video_cover: None,
                    video_timestamp: None,
                    ttl_seconds: None,
                    query: None,
                }
                .into(),
            ),
            tl::enums::Document::Empty(_) => None,
        },
        _ => None,
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_waveform_is_packed_as_it_is_unpacked() {
        let bars = vec![255u8, 0, 255, 0, 255, 0, 255, 0];
        assert_eq!(super::super::content::waveform(&packed(&bars)), bars, "the widest and the shut bar");
        // Bars between are quantized to five bits, and none louder than the widest.
        assert_eq!(super::super::content::waveform(&packed(&[10]))[0], (10u16 * 31 / 255) as u8 * 255 / 31);
        assert_eq!(packed(&[255])[0], 0b11111, "the widest bar is five set bits");
        assert!(packed(&[]).is_empty());
    }

    #[test]
    fn pasted_coordinates_are_a_place_and_a_venue_line_is_a_venue() {
        let point = pasted_place("37.7749, -122.4194").expect("coordinates are a place");
        match pasted_media(&point) {
            tl::enums::InputMedia::GeoPoint(media) => match media.geo_point {
                tl::enums::InputGeoPoint::Point(point) => {
                    assert!((point.lat - 37.7749).abs() < 1e-6);
                    assert!((point.long - -122.4194).abs() < 1e-6);
                }
                other => panic!("not a point: {other:?}"),
            },
            other => panic!("not a location: {other:?}"),
        }
        assert!(pasted_place("see you at 37.7749, -122.4194").is_none(), "words around coordinates stay text");
        assert!(pasted_place("geo:37.7955,-122.3937").is_some());
        let venue = pasted_place("venue: Ferry Building | 1 The Embarcadero | 37.7955, -122.3937").expect("a venue line is a venue");
        match pasted_media(&venue) {
            tl::enums::InputMedia::Venue(media) => {
                assert_eq!(media.title, "Ferry Building");
                assert_eq!(media.address, "1 The Embarcadero");
            }
            other => panic!("not a venue: {other:?}"),
        }
    }

    #[test]
    fn a_lone_dice_emoji_is_sent_as_a_roll() {
        assert_eq!(dice_emoticon("\u{1F3B2}"), Some("\u{1F3B2}"));
        assert_eq!(dice_emoticon("  \u{26BD}\u{FE0F} "), Some("\u{26BD}"));
        assert_eq!(dice_emoticon("\u{1F3AF}"), Some("\u{1F3AF}"));
        assert_eq!(dice_emoticon("\u{1F3C0}"), Some("\u{1F3C0}"));
        assert_eq!(dice_emoticon("\u{1F3B3}"), Some("\u{1F3B3}"));
        assert_eq!(dice_emoticon("\u{1F3B0}"), Some("\u{1F3B0}"));
        assert_eq!(dice_emoticon("\u{1F3B2} 5"), None);
        assert_eq!(dice_emoticon("hello"), None);
        assert_eq!(dice_emoticon("\u{1F3B2}\u{1F3B2}"), None);

        let target = Target {
            who: Who::User(7_002),
            peer: tl::types::InputPeerUser { user_id: 7_002, access_hash: 1 }.into(),
            reply_to: None,
        };
        let opts = Options { silent: false, no_webpage: false, schedule: None };
        let request = send_media(&target, tl::types::InputMediaDice { emoticon: "\u{1F3B2}".into() }.into(), "", None, 1, opts);
        match request.media {
            tl::enums::InputMedia::Dice(dice) => assert_eq!(dice.emoticon, "\u{1F3B2}"),
            other => panic!("not a dice roll: {other:?}"),
        }
        assert!(request.message.is_empty(), "the emoji is the roll, not a caption");
    }
}

