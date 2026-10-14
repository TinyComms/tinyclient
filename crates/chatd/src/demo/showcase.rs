//! `#showcase` in Moss & Lichen: one of everything a message can hold, in an order that reads
//! as a conversation. The read marker sits part of the way down, so there is a "New" divider.

use super::discord::{self, Discord, BOT, HARUTO, INES, TOMASZ, WREN};
use super::png::Art;
use super::serve::wav_bytes;
use super::{Fallible, Rich, Seeder, Who};
use crate::store::ConversationRow;
use protocol::{
    AccountKey, Attachment, Card, ConversationKind, Forwarded, Key, MediaKind, MentionKind, Message,
    MessageKind, Preview, Reaction, ReplyTo, SendState, Span, Style, ThreadSummary,
};

/// The message that is still being sent, which only the running demo can keep that way, and
/// the unread state its arrival would otherwise clear.
pub(crate) struct Pending {
    pub conversation: Key,
    pub message: Message,
    pub unread: u32,
    pub mentions: u32,
}

/// A thread hanging off a message of the showcase, as a row that names its parent and root.
fn thread_row(id: &str, title: &str, root: &str, position: i32, group: Option<String>) -> ConversationRow {
    ConversationRow {
        space: Some(discord::MOSS.to_owned()),
        group,
        position,
        parent: Some(discord::SHOWCASE.to_owned()),
        root: Some(root.to_owned()),
        ..ConversationRow::new(id, ConversationKind::Thread, title)
    }
}

/// A voice message's loudness, as the network would send it: a few words and the gaps between.
fn waveform(bars: usize) -> Vec<u8> {
    (0..bars)
        .map(|bar| {
            let swell = ((bar as f32 / bars as f32) * std::f32::consts::PI * 5.0).sin().abs();
            (20.0 + swell * 200.0 * (0.6 + (bar % 7) as f32 / 18.0)) as u8
        })
        .collect()
}

/// An image as a message carries it: a thumbnail on disk and an original nobody has fetched.
fn photo(s: &mut Seeder, account: &AccountKey, name: &str, art: Art, width: u32, height: u32) -> Fallible<Attachment> {
    let id = format!("att-{:08x}", s.roll(u32::MAX as u64));
    let look = s.look(art);
    let thumb = s.picture(account, &format!("{id}-thumb"), MediaKind::Image, look, width, height)?;
    let scale = (2400 / width.max(height)).clamp(1, 3);
    let file = s.waiting(account, &id, MediaKind::Image, look, width * scale, height * scale)?;
    Ok(Attachment {
        thumb: Some(thumb),
        mime: "image/png".to_owned(),
        ..Attachment::new(name, file)
    })
}

fn mention(text: &str, name: &str, kind: MentionKind, id: &str) -> Span {
    let start = text.find(name).expect("the name is in the text");
    Span {
        start: start as u32,
        end: (start + name.len()) as u32,
        style: Style::Mention { kind, id: id.to_owned() },
    }
}

fn system(mut message: Message) -> Message {
    message.kind = MessageKind::System;
    message
}

pub fn seed(s: &mut Seeder, discord: &Discord) -> Fallible<Pending> {
    let account = &discord.account;
    let room = Key::new(account, discord::SHOWCASE);
    let who = |at: usize| -> &Who { &discord.cast[at] };
    let me = &discord.me;
    let mut feed: Vec<Message> = Vec::new();
    // Minutes ago. Each message steps it towards now.
    let mut clock = 3000;
    let mut step = |minutes: i64| {
        clock -= minutes;
        clock
    };

    // Two days ago: text in every style.
    feed.push(system(s.message(account, who(HARUTO), step(0), "Haruto Senda joined the server.")));
    let styles = Rich::default()
        .plain("Text can be ")
        .styled("bold", Style::Bold)
        .plain(", ")
        .styled("italic", Style::Italic)
        .plain(", ")
        .styled("underlined", Style::Underline)
        .plain(", ")
        .styled("struck through", Style::Strike)
        .plain(", ")
        .styled("inline code", Style::Code)
        .plain(", ")
        .nested("bold and italic at once", [Style::Bold, Style::Italic])
        .plain(", or a ")
        .styled("spoiler: the marten did it", Style::Spoiler)
        .plain(".");
    feed.push(styles.into(s.message(account, who(INES), step(6), "")));

    let code = Rich::default().plain("The trigger loop on the camera, for anyone curious:\n").styled(
        "fn main() -> ! {\n    let mut pir = Sensor::new(Pin::D2);\n    loop {\n        if pir.motion() {\n            camera::capture(Frames(14));\n        }\n        sleep_ms(250);\n    }\n}",
        Style::CodeBlock { language: "rust".to_owned() },
    );
    feed.push(code.into(s.message(account, who(TOMASZ), step(9), "")));

    let quoted = Rich::default()
        .styled("Mosses have no roots, only rhizoids.\nThey drink through their leaves.", Style::Quote)
        .plain("\nWorth remembering before anyone waters the terrarium again. What to bring on Saturday:\n")
        .styled("Hand lens, 10x", Style::ListItem { ordered: false, depth: 0 })
        .plain("\n")
        .styled("Paper packets, not plastic", Style::ListItem { ordered: false, depth: 0 })
        .plain("\n")
        .styled("One per specimen", Style::ListItem { ordered: false, depth: 1 })
        .plain("\n")
        .styled("A pencil", Style::ListItem { ordered: false, depth: 0 })
        .plain("\nAnd the order of the day:\n")
        .styled("Meet at the usual corner", Style::ListItem { ordered: true, depth: 0 })
        .plain("\n")
        .styled("Creek route to the weir", Style::ListItem { ordered: true, depth: 0 })
        .plain("\n")
        .styled("Lunch, then identification", Style::ListItem { ordered: true, depth: 0 });
    feed.push(quoted.into(s.message(account, who(INES), step(7), "")));

    let links = Rich::default()
        .plain("Field key is at ")
        .styled("https://example.org/keys/bryophytes", Style::Link { url: "https://example.org/keys/bryophytes".to_owned() })
        .plain(" and the sign-up sheet is ")
        .styled("here", Style::Link { url: "https://example.org/foray/sign-up".to_owned() })
        .plain(".");
    feed.push(links.into(s.message(account, who(WREN), step(11), "")));

    let parrot = s.look(Art::Blob);
    let parrot = s.picture(account, "emoji-mossparrot", MediaKind::Emoji, parrot, 48, 48)?;
    let tick = s.look(Art::Blob);
    let tick = s.picture(account, "emoji-lichen_ok", MediaKind::Emoji, tick, 48, 48)?;
    let emoji = Rich::default()
        .plain("Camera 2 is back up ")
        .styled(":mossparrot:", Style::Emoji { file: parrot.clone() })
        .plain(" and the card was fine ")
        .styled(":lichen_ok:", Style::Emoji { file: tick.clone() })
        .plain(" 🌿");
    feed.push(emoji.into(s.message(account, who(TOMASZ), step(14), "")));
    feed.push(s.message(account, who(WREN), step(2), "🎉🌿🔥"));
    let jumbo = Rich::default().styled(":mossparrot:", Style::Emoji { file: parrot.clone() });
    feed.push(jumbo.into(s.message(account, who(HARUTO), step(1), "")));

    // Yesterday: pictures and files.
    let mut single = s.message(account, who(INES), step(1300), "Frost on the cushion moss this morning.");
    single.attachments.push(photo(s, account, "frost-cushion.png", Art::Scene, 800, 600)?);
    let single_id = single.id.clone();
    feed.push(single);
    for (count, text) in [(2, "Before and after the thaw."), (3, "Three from the weir."), (4, "The fence post, from every side."), (6, "Everything from camera 2 last night.")] {
        let mut grid = s.message(account, who(if count % 2 == 0 { WREN } else { TOMASZ }), step(16), text);
        for n in 0..count {
            let (width, height) = [(400, 300), (300, 400), (420, 280)][(n + count) % 3];
            grid.attachments.push(photo(s, account, &format!("IMG_{}.png", 4100 + count * 10 + n), Art::Scene, width, height)?);
        }
        feed.push(grid);
    }
    let mut tall = s.message(account, who(HARUTO), step(12), "The whole trunk, top to bottom.");
    tall.attachments.push(photo(s, account, "trunk-panorama.png", Art::Bands, 200, 1400)?);
    feed.push(tall);
    let mut wide = s.message(account, who(HARUTO), step(3), "And the ridge line.");
    wide.attachments.push(photo(s, account, "ridge-panorama.png", Art::Scene, 800, 130)?);
    feed.push(wide);
    // Hidden until asked for. The name keeps Discord's own prefix, as the network sends it.
    let mut hidden = s.message(account, who(WREN), step(2), "What camera 3 caught. Do not open before breakfast.");
    hidden.attachments.push(Attachment {
        spoiler: true,
        ..photo(s, account, "SPOILER_camera3-0412.png", Art::Scene, 640, 480)?
    });
    feed.push(hidden);

    // The moving files themselves are not served, so asking for one shows a failure.
    let look = s.look(Art::Scene);
    let mut gif = s.message(account, who(WREN), step(20), "");
    gif.attachments.push(Attachment {
        thumb: Some(s.picture(account, "att-gif-thumb", MediaKind::Image, look, 480, 270)?),
        mime: "image/gif".to_owned(),
        duration: 3,
        ..Attachment::new("marten-hop.gif", s.broken(account, "att-gif", MediaKind::Gif, 480, 270, 2_310_144)?)
    });
    feed.push(gif);
    let look = s.look(Art::Scene);
    let mut video = s.message(account, who(TOMASZ), step(9), "Full clip from 03:12.");
    video.attachments.push(Attachment {
        thumb: Some(s.picture(account, "att-video-thumb", MediaKind::Image, look, 640, 360)?),
        mime: "video/mp4".to_owned(),
        duration: 94,
        ..Attachment::new("camera2-0312.mp4", s.broken(account, "att-video", MediaKind::Video, 1920, 1080, 19_293_798)?)
    });
    feed.push(video);

    let mut files = s.message(account, who(TOMASZ), step(25), "Wiring diagram and last night's logs, as promised.");
    files.attachments.push(Attachment {
        mime: "application/pdf".to_owned(),
        ..Attachment::new("Trail camera wiring.pdf", s.file(account, "att-pdf", MediaKind::File, 188_416, "pdf/188416?wait=900")?)
    });
    files.attachments.push(Attachment {
        mime: "application/zip".to_owned(),
        ..Attachment::new("camera-logs.zip", s.file(account, "att-zip", MediaKind::File, 42_240, "zip/42240?wait=900")?)
    });
    feed.push(files);

    let mut voice = s.message(account, who(INES), step(18), "");
    voice.attachments.push(Attachment {
        mime: "audio/wav".to_owned(),
        duration: 4,
        waveform: waveform(64),
        ..Attachment::new("voice-message.wav", s.file(account, "att-voice", MediaKind::Audio, wav_bytes(4), "wav/4")?)
    });
    feed.push(voice);
    let look = s.look(Art::Blob);
    let mut sticker = s.message(account, who(WREN), step(4), "");
    let stuck = s.picture(account, "sticker-moss-wave", MediaKind::Image, look, 320, 320)?;
    // Its background is transparent: it is drawn on whatever the conversation's is.
    sticker.attachments.push(Attachment {
        thumb: Some(stuck.clone()),
        mime: "image/png".to_owned(),
        sticker: true,
        ..Attachment::new("Sticker: Moss Wave", stuck)
    });
    feed.push(sticker);

    // One picture that was never fetched and one that cannot be.
    let look = s.look(Art::Scene);
    let mut late = s.message(account, who(HARUTO), step(15), "Two more, if they load on this connection.");
    late.attachments.push(Attachment {
        thumb: Some(s.waiting(account, "att-late-thumb", MediaKind::Image, look, 800, 600)?),
        mime: "image/png".to_owned(),
        ..Attachment::new("weir-dawn.png", s.waiting(account, "att-late", MediaKind::Image, look, 1600, 1200)?)
    });
    late.attachments.push(Attachment {
        thumb: Some(s.broken(account, "att-lost-thumb", MediaKind::Image, 800, 600, 0)?),
        mime: "image/png".to_owned(),
        ..Attachment::new("weir-dusk.png", s.broken(account, "att-lost", MediaKind::Image, 1600, 1200, 0)?)
    });
    feed.push(late);

    let look = s.look(Art::Blob);
    let mut small = Rich::default()
        .plain("This is the one on the fence post, I think: ")
        .styled("https://example.org/wiki/Lepraria", Style::Link { url: "https://example.org/wiki/Lepraria".to_owned() })
        .into(s.message(account, who(WREN), step(30), ""));
    small.previews.push(Preview {
        url: "https://example.org/wiki/Lepraria".to_owned(),
        site: "Field Encyclopedia".to_owned(),
        title: "Lepraria".to_owned(),
        description: "A genus of leprose crustose lichens that grow on rock, bark and soil as patches of powdery granules.".to_owned(),
        image: Some(s.picture(account, "preview-small", MediaKind::Image, look, 120, 120)?),
    });
    feed.push(small);
    let look = s.look(Art::Bands);
    let mut large = Rich::default()
        .plain("And the county wrote up last year's foray: ")
        .styled("https://example.org/news/spring-foray", Style::Link { url: "https://example.org/news/spring-foray".to_owned() })
        .into(s.message(account, who(INES), step(8), ""));
    large.previews.push(Preview {
        url: "https://example.org/news/spring-foray".to_owned(),
        site: "County Naturalist".to_owned(),
        title: "Forty species in one morning on the creek route".to_owned(),
        description: "Volunteers recorded forty bryophytes and a first county record along two kilometres of bank.".to_owned(),
        image: Some(s.picture(account, "preview-large", MediaKind::Image, look, 800, 420)?),
    });
    feed.push(large);

    // Today: reactions, replies, a thread, and what happens to a message afterwards.
    let mut loved = s.message(account, who(TOMASZ), step(1150), "County recorder confirmed it: first pine marten on this ridge since 1987.");
    let counted = [("👍", 4, true), ("❤️", 12, false), ("🎉", 9, true), ("👀", 7, false), ("🔥", 5, false), ("💯", 3, false), ("😂", 3, false), ("🙏", 2, false), ("🚀", 2, false), ("🤔", 1, false), ("🌿", 1, false)];
    loved.reactions = counted
        .into_iter()
        .map(|(emoji, count, mine)| Reaction { emoji: emoji.to_owned(), file: None, count, mine })
        .collect();
    loved.reactions.insert(2, Reaction {
        emoji: ":mossparrot:".to_owned(),
        file: Some(parrot),
        count: 6,
        mine: false,
    });
    feed.push(loved);

    let mut reply = s.message(account, me, step(6), "Same spot as this one. The camera was pointing the right way for once.");
    reply.reply_to = Some(ReplyTo {
        id: single_id,
        sender: who(INES).name.clone(),
        snippet: "Frost on the cushion moss this morning.".to_owned(),
        person: Some(Key::new(account, who(INES).id.clone())),
        deleted: false,
    });
    feed.push(reply);
    // An answer to something that has since been deleted.
    let mut orphan = s.message(account, who(HARUTO), step(2), "Seconded, whatever it said.");
    orphan.reply_to = Some(ReplyTo {
        id: "1299999999999999999".to_owned(),
        sender: who(WREN).name.clone(),
        snippet: String::new(),
        person: Some(Key::new(account, who(WREN).id.clone())),
        deleted: true,
    });
    feed.push(orphan);

    let thread = Key::new(account, discord::BUILD_LOG);
    let replies = s.script(&thread, &discord.cast, me, &[
        (TOMASZ, 260, "Day 1: the sensor triggers on wind. Moved it behind the lens hood."),
        (HARUTO, 250, "Try a longer debounce before you move anything else."),
        (TOMASZ, 200, "Day 2: 400 ms debounce, no more wind, still catches the marten."),
        (usize::MAX, 190, "Battery draw?"),
        (TOMASZ, 185, "11 mA idle. A week on one pack."),
        (INES, 75, "Put the diagram in the showcase when it is done."),
    ])?;
    let mut root = s.message(account, who(TOMASZ), step(12), "Trail camera build log. Notes go in the thread so the channel stays readable.");
    let face = |at: usize| Key::new(account, who(at).id.clone());
    root.thread = Some(ThreadSummary {
        id: thread.id.clone(),
        replies: replies.len() as u32,
        last: s.now - 75 * 60,
        participants: vec![face(INES), face(TOMASZ), face(HARUTO)],
        unread: 2,
    });
    // The thread's row already sits in the tree, under the showcase; now it can say what it
    // hangs off.
    let (group, position) = s.store.conversation(&thread)?.map_or((None, 7), |row| (row.group, row.position));
    s.conversation(account, thread_row(&thread.id, "Trail camera build log", &root.id, position, group.clone()))?;
    s.counts(&thread, 2, 0, Some(&replies[3]))?;
    feed.push(root);

    // A thread whose first message was deleted: the message stays as a tombstone.
    let mut gone = s.message(account, who(WREN), step(5), "");
    gone.deleted = true;
    let aftermath = s.conversation(account, thread_row("400000000000000901", "Deleted message", &gone.id, position, group))?;
    s.script(&aftermath, &discord.cast, me, &[
        (HARUTO, 300, "What did this say? I only saw the notification."),
        (INES, 295, "Wrong channel, apparently. Nothing to see."),
    ])?;
    gone.thread = Some(ThreadSummary {
        id: aftermath.id.clone(),
        replies: 2,
        last: s.now - 295 * 60,
        participants: vec![face(INES), face(HARUTO)],
        unread: 0,
    });
    feed.push(gone);

    // Forwarded: what was said is the message's own text; where it came from is typed.
    let mut forward = s.message(account, who(WREN), step(35), "Sign-up sheet is live. Twenty places, lodging for twelve.");
    forward.forwarded = Some(Box::new(Forwarded {
        from_title: "#announcements".to_owned(),
        from_space: String::new(),
        link: format!("https://discord.com/channels/{}/400000000000000002", discord::MOSS),
    }));
    feed.push(forward);
    let mut afar = s.message(account, who(TOMASZ), step(5), "Someone got the same sensor working with a 3.3 V regulator. Schematic in their post.");
    afar.forwarded = Some(Box::new(Forwarded {
        from_title: "#show-and-tell".to_owned(),
        from_space: "Rustaceans Anonymous".to_owned(),
        link: "https://discord.com/channels/300000000000000002/410000000000000002".to_owned(),
    }));
    afar.attachments.push(photo(s, account, "schematic.png", Art::Bands, 480, 320)?);
    feed.push(afar);

    let mut edited = s.message(account, who(HARUTO), step(9), "Microscope club is Thursday at 19:00, not 18:00.");
    edited.edited = true;
    feed.push(edited);
    // Everything after this one is unread.
    let marker = feed.len() - 1;

    let mut pinned = s.message(account, who(INES), step(60), "Saturday: usual corner, 7:30, creek route. Boots, hand lens, packed lunch.");
    pinned.pinned = true;
    feed.push(pinned);
    feed.push(system(s.message(account, who(INES), step(1), "Ines Marchetti pinned a message to this channel.")));
    feed.push(system(s.message(account, who(TOMASZ), step(40), "Tomasz Brandt started a call that lasted 14 minutes.")));
    feed.push(system(s.message(account, who(WREN), step(30), "You missed a call from Wren Halloran.")));

    // What a bot posts: a line of its own, and a card of fields under it.
    let mut report = s.message(account, who(BOT), step(30), "Nightly camera report");
    report.card = Some(Box::new(Card {
        title: "3 cameras online".to_owned(),
        text: "41 frames kept overnight. One card is nearly full and one battery will not last the week.".to_owned(),
        fields: [("Camera 1", "Card 94% full"), ("Camera 2", "Pine marten, 03:12"), ("Camera 3", "Battery 11%"), ("Next report", "Tomorrow, 06:00")]
            .map(|(label, value)| (label.to_owned(), value.to_owned()))
            .to_vec(),
        footer: "Lichen Bot · trail-cams".to_owned(),
        color: Some(0x4c_9a_6a),
    }));
    feed.push(report);

    let mut poll = s.message(account, who(INES), step(20), "Poll: Where should the autumn foray go?");
    poll.kind = MessageKind::Unsupported {
        link: format!("https://discord.com/channels/{}/{}", discord::MOSS, discord::SHOWCASE),
    };
    feed.push(poll);

    let text = "@Robin Vale can you check #trail-cams with @Field Team before Saturday? @Tomasz Brandt has the keys. Heads up @everyone: the creek route is on.";
    let mut called = s.message(account, who(INES), step(30), text);
    called.spans.push(mention(text, "#trail-cams", MentionKind::Channel, "400000000000000009"));
    called.spans.push(mention(text, "@Field Team", MentionKind::Role, "380000000000000001"));
    called.spans.push(mention(text, "@Tomasz Brandt", MentionKind::User, &who(TOMASZ).id));
    called.spans.push(mention(text, "@everyone", MentionKind::Everyone, ""));
    feed.push(called);
    feed.push(s.message(account, who(TOMASZ), step(8), "I do have the keys. I checked twice."));

    let mut failed = s.message(account, me, 2, "On it. Camera and tripod are by the door.");
    failed.id = "170000000000000001".to_owned();
    failed.client_id = Some(failed.id.clone());
    failed.state = SendState::Failed {
        reason: "Slow mode: wait 8 s before sending again.".to_owned(),
    };
    feed.push(failed);

    for message in &feed {
        s.post(&room, message)?;
    }
    let counts = |message: &&Message| !message.outgoing && message.kind != MessageKind::System;
    let unread = feed[marker + 1..].iter().filter(counts).count() as u32;
    s.counts(&room, unread, 1, Some(&feed[marker].id))?;
    s.store.set_starred(&room, true)?;

    let mut message = s.message(account, me, 0, "Batteries too.");
    message.id = "170000000000000002".to_owned();
    message.client_id = Some(message.id.clone());
    message.state = SendState::Pending;
    Ok(Pending {
        conversation: room,
        message,
        unread,
        mentions: 1,
    })
}
