use super::*;
use notifier::Fake;
use protocol::settings::{account_scope, conversation_scope};

const NOW: i64 = 1_800_000_000;

fn key(network: Network, id: &str) -> Key {
    Key { network, account: "A1".into(), id: id.into() }
}

fn row(network: Network, id: &str, kind: ConversationKind, title: &str) -> Conversation {
    serde_json::from_value(serde_json::json!({
        "key": key(network, id),
        "kind": kind,
        "title": title,
        "space_title": if kind.is_direct() { "" } else { "Northwind" },
    }))
    .expect("a conversation")
}

fn dm() -> Conversation {
    row(Network::Slack, "D1", ConversationKind::Direct, "Ines Varga")
}

fn channel() -> Conversation {
    row(Network::Slack, "C1", ConversationKind::Channel, "platform")
}

fn message(from: &str, text: &str) -> Message {
    serde_json::from_value(serde_json::json!({ "id": format!("m-{text}"), "sender": { "id": "u1", "name": from }, "sent": NOW, "text": text }))
        .expect("a message")
}

fn mention(from: &str, text: &str) -> Message {
    Message { mentions_me: true, ..message(from, text) }
}

fn moment() -> Moment {
    Moment { now: NOW, minute: 12 * 60, ..Moment::default() }
}

fn set(values: &mut Values, scope: &str, key: &str, value: &str) {
    assert!(settings::valid(scope, key, value), "{scope} {key} {value}");
    values.set(scope, key, Some(value.to_owned()));
}

fn posts(values: &Values, conversation: &Conversation, message: &Message, moment: &Moment) -> bool {
    decide(values, conversation, message, moment).is_some()
}

fn sound(values: &Values, conversation: &Conversation, message: &Message, moment: &Moment) -> Option<String> {
    decide(values, conversation, message, moment).and_then(|verdict| verdict.sound)
}

#[test]
fn by_default_direct_messages_and_mentions_notify_with_pebble_and_channel_chatter_does_not() {
    let values = Values::default();
    let now = moment();
    assert_eq!(sound(&values, &dm(), &message("Ines", "hi"), &now).as_deref(), Some("pebble"));
    assert!(!posts(&values, &channel(), &message("Tom", "lunch?"), &now));
    assert!(posts(&values, &channel(), &mention("Tom", "@me look"), &now));
    let group = row(Network::Slack, "G1", ConversationKind::Group, "Lisbon trip");
    assert!(posts(&values, &group, &message("Odile", "tickets"), &now));
}

#[test]
fn never_my_own_messages_history_at_connect_or_the_conversation_being_read() {
    let values = Values::default();
    let mine = Message { outgoing: true, ..message("Me", "sent from my phone") };
    assert!(!posts(&values, &dm(), &mine, &moment()));
    let old = Message { sent: NOW - STALE - 1, ..message("Ines", "from yesterday") };
    assert!(!posts(&values, &dm(), &old, &moment()));
    let deleted = Message { deleted: true, ..message("Ines", "oops") };
    assert!(!posts(&values, &dm(), &deleted, &moment()));
    // Open and focused: being read. Open but in the background: it notifies.
    let mut reading = moment();
    reading.watching = Watching { conversation: Some(dm().key), focused: true, ..Watching::default() };
    assert!(!posts(&values, &dm(), &message("Ines", "hi"), &reading));
    reading.watching.focused = false;
    assert!(posts(&values, &dm(), &message("Ines", "hi"), &reading));
}

#[test]
fn focus_on_another_conversation_gives_a_banner_without_a_sound() {
    let values = Values::default();
    let mut elsewhere = moment();
    elsewhere.watching = Watching { conversation: Some(channel().key), focused: true, ..Watching::default() };
    let verdict = decide(&values, &dm(), &message("Ines", "hi"), &elsewhere).expect("a banner");
    assert_eq!(verdict.sound, None);
}

#[test]
fn muted_hidden_requests_and_the_master_switch_never_notify() {
    let mut values = Values::default();
    let now = moment();
    let muted = Conversation { muted: true, ..dm() };
    assert!(!posts(&values, &muted, &mention("Ines", "hi"), &now));
    let hidden = Conversation { hidden: true, ..dm() };
    assert!(!posts(&values, &hidden, &message("Ines", "hi"), &now));
    let request = Conversation { special: Special::Request, ..dm() };
    assert!(!posts(&values, &request, &message("Stranger", "hi"), &now));
    set(&mut values, GLOBAL, settings::NOTIFY, "off");
    assert!(!posts(&values, &dm(), &mention("Ines", "hi"), &now));
}

#[test]
fn bots_and_the_archive_notify_only_for_mentions() {
    let values = Values::default();
    let now = moment();
    let bot = Conversation { bot: true, ..dm() };
    assert!(!posts(&values, &bot, &message("Deploy bot", "build green"), &now));
    assert!(posts(&values, &bot, &mention("Deploy bot", "@me approve"), &now));
    let archived = Conversation { archived: true, ..dm() };
    assert!(!posts(&values, &archived, &message("Ines", "hi"), &now));
}

#[test]
fn all_messages_is_global_then_per_account_then_per_conversation() {
    let mut values = Values::default();
    let now = moment();
    let chatter = message("Tom", "lunch?");
    set(&mut values, GLOBAL, settings::NOTIFY_ABOUT, "all");
    assert!(posts(&values, &channel(), &chatter, &now));
    // The account says mentions only: its channels go quiet, its DMs still notify.
    let account = account_scope(&channel().key.account());
    set(&mut values, &account, settings::NOTIFY_ABOUT, "mentions");
    assert!(!posts(&values, &channel(), &chatter, &now) && posts(&values, &dm(), &message("Ines", "hi"), &now));
    // The conversation says all: it beats its account.
    let here = conversation_scope(&channel().key);
    set(&mut values, &here, settings::NOTIFY_ABOUT, "all");
    assert!(posts(&values, &channel(), &chatter, &now));
    // A DM set to mentions only means it.
    set(&mut values, &conversation_scope(&dm().key), settings::NOTIFY_ABOUT, "mentions");
    assert!(!posts(&values, &dm(), &message("Ines", "hi"), &now) && posts(&values, &dm(), &mention("Ines", "@me"), &now));
    // Off, for the account or the conversation, is nothing at all.
    set(&mut values, &here, settings::NOTIFY_ABOUT, "off");
    assert!(!posts(&values, &channel(), &mention("Tom", "@me"), &now));
    set(&mut values, &account, settings::NOTIFY_ABOUT, "off");
    assert!(!posts(&values, &row(Network::Slack, "C2", ConversationKind::Channel, "random"), &mention("Tom", "@me"), &now));
}

#[test]
fn each_kind_has_its_own_switch_and_tiny_servers_one_more() {
    let mut values = Values::default();
    let now = moment();
    set(&mut values, GLOBAL, settings::NOTIFY_DIRECT, "off");
    assert!(!posts(&values, &dm(), &message("Ines", "hi"), &now));
    let group = row(Network::Slack, "G1", ConversationKind::Group, "Lisbon trip");
    assert!(posts(&values, &group, &message("Odile", "tickets"), &now));
    set(&mut values, GLOBAL, settings::NOTIFY_GROUPS, "off");
    assert!(!posts(&values, &group, &message("Odile", "tickets"), &now));
    set(&mut values, GLOBAL, settings::NOTIFY_MENTIONS, "off");
    assert!(!posts(&values, &channel(), &mention("Tom", "@me"), &now));

    let values = Values::default();
    let room = row(Network::Tiny, "R1", ConversationKind::Channel, "lobby");
    assert!(posts(&values, &room, &mention("Kai", "@me"), &now));
    let mut values = Values::default();
    set(&mut values, GLOBAL, settings::NOTIFY_TINY, "off");
    assert!(!posts(&values, &room, &mention("Kai", "@me"), &now));
    // A direct message on a Tiny server is a direct message.
    assert!(posts(&values, &row(Network::Tiny, "D9", ConversationKind::Direct, "Kai"), &message("Kai", "hi"), &now));
}

#[test]
fn quiet_hours_cross_midnight_and_a_pause_holds_until_its_time() {
    assert!(in_window(22 * 60, 8 * 60, 23 * 60) && in_window(22 * 60, 8 * 60, 7 * 60 + 59));
    assert!(!in_window(22 * 60, 8 * 60, 8 * 60) && !in_window(22 * 60, 8 * 60, 12 * 60));
    assert!(in_window(9 * 60, 17 * 60, 9 * 60) && !in_window(9 * 60, 17 * 60, 17 * 60));
    assert!(!in_window(600, 600, 600), "an empty window holds nothing");

    let mut values = Values::default();
    let hi = message("Ines", "hi");
    set(&mut values, GLOBAL, settings::QUIET_HOURS, "on");
    let night = Moment { minute: 23 * 60, ..moment() };
    assert!(!posts(&values, &dm(), &hi, &night) && posts(&values, &dm(), &hi, &moment()));

    let mut values = Values::default();
    set(&mut values, GLOBAL, settings::PAUSED_UNTIL, &(NOW + 3_600).to_string());
    assert!(!posts(&values, &dm(), &hi, &moment()));
    let later = Message { sent: NOW + 3_600, ..hi.clone() };
    assert!(posts(&values, &dm(), &later, &Moment { now: NOW + 3_600, ..moment() }));
    set(&mut values, GLOBAL, settings::PAUSED_UNTIL, "resume");
    assert!(!posts(&values, &dm(), &hi, &moment()));
}

#[test]
fn the_networks_own_do_not_disturb_is_followed() {
    let values = Values::default();
    assert!(!posts(&values, &dm(), &message("Ines", "hi"), &Moment { dnd: true, ..moment() }));
}

#[test]
fn in_a_call_banners_are_silent_unless_asked_and_can_be_off() {
    let mut values = Values::default();
    let hi = message("Ines", "hi");
    let calling = Moment { in_call: true, ..moment() };
    assert_eq!(decide(&values, &dm(), &hi, &calling).map(|v| v.sound), Some(None));
    set(&mut values, GLOBAL, settings::IN_CALL, "sounds");
    assert_eq!(sound(&values, &dm(), &hi, &calling).as_deref(), Some("pebble"));
    set(&mut values, GLOBAL, settings::IN_CALL, "nothing");
    assert!(!posts(&values, &dm(), &hi, &calling));
}

#[test]
fn sounds_follow_play_sounds_and_the_picks_from_conversation_to_account_to_global() {
    let mut values = Values::default();
    let now = moment();
    let at_me = mention("Tom", "@me");
    assert_eq!(sound(&values, &channel(), &at_me, &now).as_deref(), Some("pebble"), "same as messages");
    set(&mut values, GLOBAL, settings::MENTION_SOUND, "glass");
    assert_eq!(sound(&values, &channel(), &at_me, &now).as_deref(), Some("glass"));
    assert_eq!(sound(&values, &dm(), &message("Ines", "hi"), &now).as_deref(), Some("pebble"));
    set(&mut values, &account_scope(&dm().key.account()), settings::SOUND, "tap");
    assert_eq!(sound(&values, &dm(), &message("Ines", "hi"), &now).as_deref(), Some("tap"));
    set(&mut values, &conversation_scope(&dm().key), settings::SOUND, "chime");
    assert_eq!(sound(&values, &dm(), &message("Ines", "hi"), &now).as_deref(), Some("chime"));
    set(&mut values, &conversation_scope(&dm().key), settings::SOUND, "none");
    assert_eq!(decide(&values, &dm(), &message("Ines", "hi"), &now).map(|v| v.sound), Some(None));
    set(&mut values, GLOBAL, settings::SOUNDS, "off");
    assert_eq!(decide(&values, &channel(), &at_me, &now).map(|v| v.sound), Some(None));
}

#[test]
fn a_sound_is_not_repeated_within_ten_seconds_here_or_three_anywhere() {
    let values = Values::default();
    let hi = message("Ines", "hi");
    assert_eq!(decide(&values, &dm(), &hi, &Moment { sounded_here: true, ..moment() }).map(|v| v.sound), Some(None));
    assert_eq!(decide(&values, &dm(), &hi, &Moment { sounded_any: true, ..moment() }).map(|v| v.sound), Some(None));

    let fake = Fake::default();
    let mut notes = Notes::new(Box::new(fake.clone()));
    let verdict = decide(&values, &dm(), &hi, &moment()).unwrap();
    let start = Instant::now();
    assert_eq!(notes.message(&dm(), &hi, &verdict, start).as_deref(), Some("pebble"));
    assert_eq!(notes.quiet(&dm().key, start + Duration::from_secs(2)), (true, true));
    assert_eq!(notes.quiet(&channel().key, start + Duration::from_secs(4)), (false, false));
    assert_eq!(notes.quiet(&dm().key, start + Duration::from_secs(9)), (true, false));
    assert_eq!(notes.quiet(&dm().key, start + Duration::from_secs(11)), (false, false));
}

#[test]
fn the_preview_is_what_the_settings_allow_and_less_on_a_locked_screen() {
    let mut values = Values::default();
    assert_eq!(decide(&values, &dm(), &message("Ines", "hi"), &moment()).unwrap().preview, Preview::Full);
    assert_eq!(decide(&values, &dm(), &message("Ines", "hi"), &Moment { locked: true, ..moment() }).unwrap().preview, Preview::Sender);
    set(&mut values, GLOBAL, settings::PREVIEW_LOCKED, "nothing");
    assert_eq!(decide(&values, &dm(), &message("Ines", "hi"), &Moment { locked: true, ..moment() }).unwrap().preview, Preview::Nothing);
    set(&mut values, GLOBAL, settings::PREVIEW, "sender");
    assert_eq!(decide(&values, &dm(), &message("Ines", "hi"), &moment()).unwrap().preview, Preview::Sender);
}

fn post(notes: &mut Notes, conversation: &Conversation, message: &Message, preview: Preview) {
    let verdict = Verdict { banner: true, sound: None, preview, mention: message.mentions_me };
    notes.message(conversation, message, &verdict, Instant::now());
}

#[test]
fn one_notification_per_conversation_with_its_newest_three_lines() {
    let fake = Fake::default();
    let mut notes = Notes::new(Box::new(fake.clone()));
    let id = note_id(&dm().key);
    post(&mut notes, &dm(), &message("Ines Varga", "are we on"), Preview::Full);
    {
        let log = fake.log();
        let shown = &log.shown[&id];
        assert_eq!((shown.title.as_str(), shown.subtitle.as_str(), shown.body.as_str()), ("Ines Varga", "", "are we on"));
        assert_eq!((shown.kind, shown.reply_hint.as_str()), (Kind::Message, "Reply to Ines"));
    }
    for text in ["for 3", "at the cafe", "bring the slides"] {
        post(&mut notes, &dm(), &message("Ines Varga", text), Preview::Full);
    }
    assert_eq!(fake.log().shown[&id].body, "for 3\nat the cafe\nbring the slides");
    assert_eq!(fake.log().shown.len(), 1, "replaced in place");
    // Several senders: each line says who.
    post(&mut notes, &channel(), &mention("Tom Brandt", "@me deploy?"), Preview::Full);
    post(&mut notes, &channel(), &mention("Odile Ferrand", "+1"), Preview::Full);
    let channel_id = note_id(&channel().key);
    let shown = fake.log().shown[&channel_id].clone();
    assert_eq!((shown.title.as_str(), shown.subtitle.as_str()), ("Odile Ferrand", "#platform · Northwind"));
    assert_eq!(shown.body, "Tom Brandt: @me deploy?\nOdile Ferrand: +1");
    assert_eq!(notes.conversation(&channel_id), Some(&channel().key));
}

#[test]
fn sender_only_and_nothing_keep_the_text_off_the_screen() {
    let fake = Fake::default();
    let mut notes = Notes::new(Box::new(fake.clone()));
    post(&mut notes, &channel(), &mention("Tom Brandt", "the password is hunter2"), Preview::Sender);
    let shown = fake.log().shown[&note_id(&channel().key)].clone();
    assert_eq!((shown.title.as_str(), shown.body.as_str()), ("Tom Brandt", "New mention"));
    post(&mut notes, &dm(), &message("Ines Varga", "secret"), Preview::Nothing);
    let shown = fake.log().shown[&note_id(&dm().key)].clone();
    assert_eq!((shown.title.as_str(), shown.subtitle.as_str(), shown.body.as_str()), ("Slack", "", "New message"));
    assert!(fake.log().posted.iter().all(|note| !note.body.contains("hunter2") && !note.body.contains("secret")));
}

#[test]
fn reading_the_conversation_anywhere_withdraws_it_and_starts_its_lines_over() {
    let fake = Fake::default();
    let mut notes = Notes::new(Box::new(fake.clone()));
    post(&mut notes, &dm(), &message("Ines Varga", "one"), Preview::Full);
    assert!(notes.has(&dm().key));
    notes.read(&dm().key);
    notes.read(&dm().key);
    assert_eq!(fake.log().withdrawn, [note_id(&dm().key)]);
    assert!(!notes.has(&dm().key) && notes.conversation(&note_id(&dm().key)).is_none());
    post(&mut notes, &dm(), &message("Ines Varga", "two"), Preview::Full);
    assert_eq!(fake.log().shown[&note_id(&dm().key)].body, "two");
}

#[test]
fn a_refused_notification_plays_no_sound() {
    let fake = Fake::default();
    fake.log().refuse = Some("turned off".into());
    let mut notes = Notes::new(Box::new(fake.clone()));
    let verdict = Verdict { banner: true, sound: Some("pebble".into()), preview: Preview::Full, mention: false };
    assert_eq!(notes.message(&dm(), &message("Ines", "hi"), &verdict, Instant::now()), None);
    assert_eq!(notes.test(&Values::default(), false).0, false);
}

fn ringing(conversation: &Key, from: &str) -> Call {
    serde_json::from_value(serde_json::json!({
        "conversation": conversation,
        "state": "ringing_in",
        "from": from,
        "participants": [],
        "muted": false,
        "deafened": false,
        "joined": 0,
        "engine_linked": false,
        "video": [],
    }))
    .expect("a call")
}

#[test]
fn a_ring_follows_incoming_calls_and_quiet_time_lets_it_through_by_default() {
    let mut values = Values::default();
    let room = key(Network::Discord, "D7");
    assert_eq!(decide_ring(&values, &room, &moment()).and_then(|v| v.sound).as_deref(), Some("soft_ring"));
    let paused = Moment { now: NOW, ..moment() };
    set(&mut values, GLOBAL, settings::PAUSED_UNTIL, "resume");
    assert!(decide_ring(&values, &room, &paused).is_some(), "N9 is on");
    set(&mut values, GLOBAL, settings::CALLS_THROUGH, "off");
    assert!(decide_ring(&values, &room, &paused).is_none());

    let mut values = Values::default();
    set(&mut values, GLOBAL, settings::CALLS, "silent");
    assert_eq!(decide_ring(&values, &room, &moment()).map(|v| v.sound), Some(None));
    set(&mut values, GLOBAL, settings::CALLS, "off");
    assert!(decide_ring(&values, &room, &moment()).is_none());

    let mut values = Values::default();
    set(&mut values, GLOBAL, settings::SOUNDS, "off");
    assert_eq!(decide_ring(&values, &room, &moment()).map(|v| v.sound), Some(None));
    let values = Values::default();
    // The window has focus: its own card shows the call, so no banner, but it is heard.
    let focused = Moment { watching: Watching { focused: true, ..Watching::default() }, ..moment() };
    let verdict = decide_ring(&values, &room, &focused).unwrap();
    assert!(!verdict.banner && verdict.sound.is_some());
}

#[test]
fn a_ring_is_posted_once_with_accept_and_decline_and_withdrawn_when_it_stops() {
    let fake = Fake::default();
    let mut notes = Notes::new(Box::new(fake.clone()));
    let room = key(Network::Discord, "D7");
    let call = ringing(&room, "Ines Varga");
    let verdict = || decide_ring(&Values::default(), &room, &moment());
    assert_eq!(notes.call(Some(&call), verdict(), "", "Discord").as_deref(), Some("soft_ring"));
    assert_eq!(notes.call(Some(&call), verdict(), "", "Discord"), None, "the same ring");
    let shown: Vec<Note> = fake.log().shown.values().cloned().collect();
    assert_eq!(shown.len(), 1);
    assert_eq!((shown[0].title.as_str(), shown[0].subtitle.as_str(), shown[0].kind), ("Ines Varga", "Discord · Incoming call", Kind::Call));
    let id = shown[0].id.clone();
    assert_eq!(notes.conversation(&id), Some(&room));
    // Answered elsewhere, declined, or the caller gave up: withdrawn, and the ring stops.
    assert_eq!(notes.call(None, None, "", "").as_deref(), Some(""));
    assert!(fake.log().shown.is_empty() && notes.conversation(&id).is_none());
    assert_eq!(notes.call(None, None, "", ""), None);
}

#[test]
fn the_test_notification_uses_the_preview_and_the_message_sound() {
    let fake = Fake::default();
    let mut notes = Notes::new(Box::new(fake.clone()));
    let (posted, detail, sound) = notes.test(&Values::default(), false);
    assert!(posted && detail.is_empty());
    assert_eq!(sound.as_deref(), if notifier::SYSTEM_PLAYS_SOUND { None } else { Some("pebble") });
    assert_eq!(fake.log().shown["test"].body, "This is how messages will look and sound.");
    let mut values = Values::default();
    set(&mut values, GLOBAL, settings::PREVIEW, "nothing");
    set(&mut values, GLOBAL, settings::SOUNDS, "off");
    let (_, _, sound) = notes.test(&values, false);
    assert_eq!((fake.log().shown["test"].body.as_str(), sound), ("New message", None));
}

#[test]
fn the_bundled_files_are_named_as_the_bundle_carries_them() {
    assert_eq!(sound_file("pebble").as_deref(), Some("tinyclient-pebble"));
    assert_eq!(sound_file("soft_ring").as_deref(), Some("tinyclient-soft-ring"));
    assert_eq!(sound_file("none"), None);
    let minute = local_minute(NOW);
    assert!(minute < 24 * 60);
}
