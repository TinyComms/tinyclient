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
use protocol::{Message, Outgoing};
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
    if with.files.is_empty() {
        let outcome = match &with.pick {
            Some(pick) => send_pick(account, conversation, client_id, text, reply_to, &pick.id, opts),
            None => super::send_text(account, conversation, client_id, text, reply_to, opts),
        };
        ctx.sent(conversation, client_id, outcome.map_err(|fault| fault.words()));
        return;
    }
    let (account, conversation_id, client, text, reply) =
        (account.clone(), conversation.to_owned(), client_id.to_owned(), text.to_owned(), reply_to.map(str::to_owned));
    upload::spawn(&ctx, conversation, client_id, &with.files, move |up| {
        files(&account, &conversation_id, &client, &text, reply.as_deref(), opts, up)
    });
}

pub(super) fn cancel(account: &Account, client_id: &str) {
    upload::cancel(&account.ctx.account, client_id);
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
    let (answer, caption) = match catalog::find(account, pick).ok_or_else(|| Fault::Rpc { code: 400, name: "PICK_EXPIRED".to_owned(), value: None })? {
        Pick::Document(id) => {
            remember_random(account, random, client_id);
            let media = tl::types::InputMediaDocument { spoiler: false, id: id.into(), video_cover: None, video_timestamp: None, ttl_seconds: None, query: None };
            (account.ask(&send_media(&target, media.into(), text, random, opts))?, text)
        }
        Pick::Inline { query_id, result } => {
            // A bot's result carries no caption: words typed with it go first, on their own.
            if !text.trim().is_empty() {
                super::send_text(account, conversation, &format!("{client_id}1"), text, None, opts)?;
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

fn send_media(target: &Target, media: tl::enums::InputMedia, caption: &str, random_id: i64, opts: Options) -> tl::functions::messages::SendMedia {
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
        entities: None,
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

fn files(account: &Account, conversation: &str, client_id: &str, text: &str, reply_to: Option<&str>, opts: Options, up: &mut Upload) -> Result<Message, String> {
    let words = |fault: Fault| fault.words();
    let target = target(account, conversation, reply_to).map_err(words)?;
    let premium = account.dir().get(Who::User(account.me)).is_some_and(|known| known.premium);
    let limit = if premium { 4_096 } else { 1_024 };
    // A caption longer than Telegram takes goes first as a message of its own.
    let caption = if text.chars().count() > limit {
        super::send_text(account, conversation, &format!("{client_id}1"), text, reply_to, opts).map_err(words)?;
        ""
    } else {
        text
    };
    let mut uploaded = Vec::with_capacity(up.files().len());
    for at in 0..up.files().len() {
        uploaded.push(file(account, client_id, at, up)?);
    }
    up.check()?;
    let random = random_id(client_id);
    remember_random(account, random, client_id);
    if uploaded.len() == 1 {
        let Some(one) = uploaded.pop() else {
            return Err("Nothing was sent.".to_owned());
        };
        let answer = account.ask(&send_media(&target, one.media, caption, random, opts)).map_err(words)?;
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
                index += 1;
                let answer = account.ask(&send_media(&target, one.media.clone(), shown, item_random, opts)).map_err(words)?;
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
                    entities: None,
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

/// One file of a send, in parts, as Telegram's `InputMedia`.
fn file(account: &Account, client_id: &str, at: usize, up: &mut Upload) -> Result<Uploaded, String> {
    let (name, bytes, mime, spoiler, as_file) = {
        let source = &up.files()[at];
        (source.name.clone(), source.bytes, source.mime, source.spoiler, source.as_file)
    };
    let file_id = random_id(&format!("{client_id}/{at}/{name}"));
    let big = bytes > BIG;
    let mut parts = 1;
    up.parts(at, PART, 0, |part| {
        parts = part.count as i32;
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
    let photo = !as_file && PHOTOS.contains(&mime) && bytes <= BIG;
    let media: tl::enums::InputMedia = match photo {
        true => tl::types::InputMediaUploadedPhoto { spoiler, live_photo: false, file: input, stickers: None, ttl_seconds: None, video: None }.into(),
        false => tl::types::InputMediaUploadedDocument {
            nosound_video: false,
            force_file: as_file,
            spoiler: spoiler && !as_file,
            file: input,
            thumb: None,
            mime_type: mime.to_owned(),
            attributes: vec![tl::types::DocumentAttributeFilename { file_name: name }.into()],
            stickers: None,
            video_cover: None,
            video_timestamp: None,
            ttl_seconds: None,
        }
        .into(),
    };
    Ok(Uploaded { media, visual: photo || (!as_file && mime.starts_with("video/")) })
}

/// What `messages.uploadMedia` kept, as media an album can name, with the spoiler kept.
fn stored(held: tl::enums::MessageMedia, asked: &tl::enums::InputMedia) -> Option<tl::enums::InputMedia> {
    let spoiler = match asked {
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
