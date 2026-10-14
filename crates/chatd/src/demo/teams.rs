//! The demo's Teams account: chats that belong to no Space, two teams with shown and hidden
//! channels, a channel post with replies, and the presence Teams is known for (Busy in a
//! meeting, presenting, out of office).

use super::png::Art;
use super::{Fallible, Listed, Seeder, Who};
use crate::store::{ConversationRow, Filing, PersonDetails, PreviewLine, SpaceRow};
use protocol::{
    AccountKey, Activity, ActivityKind, CallSupport, ConversationKind, Key, Mark, MediaKind, Network, Presence,
    Relation, Special, ThreadSummary,
};
use ConversationKind::{Channel, Direct, Group, Thread};
use Presence::{Busy, Dnd, Idle, Offline, Online};
use Relation::Contact;

pub const ME: &str = "8:orgid:robin";
pub const ENGINEERING: &str = "19:team-engineering";
pub const SOCIAL: &str = "19:team-social";

pub fn account() -> AccountKey {
    AccountKey {
        network: Network::Teams,
        account: "harbor-tenant".to_owned(),
    }
}

const PEOPLE: &[Listed] = &[
    ("Ana Ruiz", "ana.ruiz@harbor.example", Busy, "In a meeting", Contact),
    ("Ben Okoro", "ben.okoro@harbor.example", Online, "", Contact),
    ("Chloé Martin", "chloe.martin@harbor.example", Offline, "Out of office · until Fri", Contact),
    ("Dmitri Volkov", "dmitri.volkov@harbor.example", Idle, "", Contact),
    ("Esi Mensah", "esi.mensah@harbor.example", Dnd, "Focusing", Contact),
    ("Farah Siddiqui", "farah@fabrikam.example", Busy, "Presenting", Contact),
    ("Workflows", "workflows-bot", Presence::Unknown, "", Relation::None),
    ("Viva Insights", "viva-bot", Presence::Unknown, "", Relation::None),
];
const ANA: usize = 0;
const BEN: usize = 1;
const CHLOE: usize = 2;
const DMITRI: usize = 3;
const ESI: usize = 4;
const FARAH: usize = 5;
const WORKFLOWS: usize = 6;
const VIVA: usize = 7;

const DAY: i64 = 1_440;

pub fn seed(s: &mut Seeder) -> Fallible<Who> {
    let account = account();
    s.store.set_account(&account, "Harbor Labs", ME)?;
    let look = s.look(Art::Person);
    let me = Who {
        id: ME.to_owned(),
        name: "Robin Vale".to_owned(),
        avatar: Some(s.picture(&account, "teams-avatar-me", MediaKind::Avatar, look, 80, 80)?),
        bot: false,
        me: true,
    };
    s.myself(&account, &me)?;
    let cast = s.people(&account, "8:orgid:p", PEOPLE)?;
    let titles = [
        (ANA, "Engineering manager", -18_000),
        (BEN, "Staff engineer", 3_600),
        (CHLOE, "Product designer", 3_600),
        (DMITRI, "SRE", 10_800),
        (ESI, "Data scientist", 0),
        (FARAH, "Partner architect", 19_800),
    ];
    let mut details: Vec<(String, PersonDetails)> = titles
        .iter()
        .map(|(at, title, offset)| {
            (cast[*at].id.clone(), PersonDetails { title: (*title).to_owned(), utc_offset: Some(*offset), mark: None })
        })
        .collect();
    details[FARAH].1.mark = Some(Mark::External);
    s.store.set_details(&account, &details)?;
    let doing = |kind| Some(Activity { kind, name: String::new(), place: String::new(), conversation: None });
    s.activities.push((Key::new(&account, cast[ANA].id.clone()), String::new(), doing(ActivityKind::InMeeting)));
    s.activities.push((Key::new(&account, cast[FARAH].id.clone()), String::new(), doing(ActivityKind::Presenting)));
    s.activities.push((Key::new(&account, cast[CHLOE].id.clone()), "🌴".to_owned(), None));

    let chat = |id: &str, kind, title: &str| ConversationRow { call: CallSupport::Handoff, ..ConversationRow::new(id, kind, title) };
    let direct = |at: usize| ConversationRow {
        peer: Some(cast[at].id.clone()),
        avatar: cast[at].avatar.as_ref().map(|file| file.id.clone()),
        bot: cast[at].bot,
        call: if cast[at].bot { CallSupport::None } else { CallSupport::Handoff },
        ..ConversationRow::new(format!("19:{}_robin@unq.gbl.spaces", at + 1), Direct, cast[at].name.clone())
    };
    let talk = |s: &mut Seeder, row: ConversationRow, lines: &[(usize, i64, &str)]| -> Fallible<(Key, Vec<String>)> {
        let key = s.conversation(&account, row)?;
        let said = s.script(&key, &cast, &me, lines)?;
        Ok((key, said))
    };

    let (ana, said) = talk(s, direct(ANA), &[(usize::MAX, 300, "Can we move the 1:1?"), (ANA, 12, "Yes, Thursday 10:00 works.")])?;
    s.counts(&ana, 1, 0, Some(&said[0]))?;
    let (ben, said) = talk(s, direct(BEN), &[(BEN, 90, "Is the rollout plan final?"), (usize::MAX, 60, "Final. Sharing it in Releases.")])?;
    // Seen under my last message, as Teams shows in a one-to-one chat.
    s.store.set_seen(&ben, &said[1])?;
    talk(s, direct(CHLOE), &[(CHLOE, 3 * DAY, "Handing the onboarding flows to you while I'm away.")])?;
    talk(s, direct(FARAH), &[(FARAH, 6 * DAY, "Thanks for the API walkthrough, sharing with my team.")])?;
    // An untitled group, named after its people, never after me.
    let (group, _) = talk(s, chat("19:group-untitled@thread.v2", Group, "Ana, Ben, +3"), &[
        (BEN, 200, "Lunch Friday?"),
        (DMITRI, 150, "In."),
        (usize::MAX, 140, "In."),
    ])?;
    s.counts(&group, 0, 0, None)?;
    talk(s, chat("19:platform-guild@thread.v2", Group, "Platform guild"), &[(ESI, 40, "Notebook for the latency analysis is up.")])?;

    // Meeting chats: one from this week, one quiet 20 days (the collapsed Meetings group), one
    // quiet 40 days (out of the Inbox too).
    for (id, title, days) in [("19:meeting_sprint@thread.v2", "Sprint review", 2), ("19:meeting_q3@thread.v2", "Q3 planning", 20), ("19:meeting_vendor@thread.v2", "Vendor sync", 40)] {
        talk(s, ConversationRow { meeting: true, ..chat(id, Group, title) }, &[(ANA, days * DAY, "Recording and notes are in the meeting tab.")])?;
    }

    // Apps: never people, never a number.
    let (workflows, _) = talk(s, direct(WORKFLOWS), &[(WORKFLOWS, 35, "Daily standup reminder: post your update in #engineering.")])?;
    s.counts(&workflows, 1, 0, None)?;
    talk(s, direct(VIVA), &[(VIVA, 2 * DAY, "You have 3 hours of focus time booked this week.")])?;

    // My own notes, by recency.
    talk(s, ConversationRow { special: Special::Saved, ..chat("48:notes", Direct, "Notes") }, &[(usize::MAX, 500, "Ask Ben about the canary metrics")])?;

    // Hidden in Teams: never listed.
    talk(s, ConversationRow {
        filing: Some(Filing { hidden: true, ..Filing::default() }),
        ..chat("19:old-project@thread.v2", Group, "Old project")
    }, &[(DMITRI, 60 * DAY, "Archiving this chat.")])?;

    // Two chats whose list gave only a line, as Teams' does, and two with nothing yet: those
    // are filled when they come on screen (`Command::Previews`).
    let line = |s: &mut Seeder, id: &str, title: &str, from: usize, minutes: i64, text: &str| -> Fallible<()> {
        let key = s.conversation(&account, chat(id, Group, title))?;
        let sent = s.now - minutes * 60;
        let line = PreviewLine { id: format!("{sent}000"), text: text.to_owned(), sender: cast[from].name.clone(), mine: false, sent };
        s.store.set_preview(&key, &line)?;
        Ok(())
    };
    line(s, "19:design-crit@thread.v2", "Design crit", CHLOE, 4 * DAY, "New nav mocks are in Figma")?;
    line(s, "19:oncall@thread.v2", "On-call handover", DMITRI, 7, "Pager is yours from 18:00")?;
    for (id, title) in [("19:hiring@thread.v2", "Hiring loop"), ("19:offsite@thread.v2", "Offsite planning")] {
        let key = s.conversation(&account, chat(id, Group, title))?;
        s.store.set_counts(&key, 0, 0, None, s.now - 3 * DAY * 60)?;
    }

    // Two teams. Teams has no categories: channels sit under no group, General first; the
    // channels the user hid are there for "Show hidden".
    let look = s.look(Art::Badge);
    let engineering_icon = s.picture(&account, "teams-icon-engineering", MediaKind::Icon, look, 96, 96)?;
    let look = s.look(Art::Badge);
    let social_icon = s.picture(&account, "teams-icon-social", MediaKind::Icon, look, 96, 96)?;
    s.store.replace_spaces(&account, &[
        SpaceRow { id: ENGINEERING.to_owned(), name: "Harbor Engineering".to_owned(), icon: Some(engineering_icon.id), position: 0 },
        SpaceRow { id: SOCIAL.to_owned(), name: "Harbor Social".to_owned(), icon: Some(social_icon.id), position: 1 },
    ])?;
    let channels = |team: &str, names: &[(&str, bool)]| -> Vec<ConversationRow> {
        names
            .iter()
            .enumerate()
            .map(|(position, (name, hidden))| ConversationRow {
                position: position as i32,
                opted_out: *hidden,
                call: CallSupport::Handoff,
                ..ConversationRow::new(format!("{team}-{position}"), Channel, *name)
            })
            .collect()
    };
    let engineering = channels(ENGINEERING, &[("General", false), ("Releases", false), ("Incidents", false), ("Archive 2024", true), ("Hackathon", true)]);
    let social = channels(SOCIAL, &[("General", false), ("Random", false), ("Book club", true)]);
    s.store.replace_tree(&Key::new(&account, ENGINEERING), &[], &engineering)?;
    s.store.replace_tree(&Key::new(&account, SOCIAL), &[], &social)?;

    // A channel is a list of posts; one has replies, which open as a thread.
    let general = Key::new(&account, engineering[0].id.clone());
    let mut post = s.message(&account, &cast[ANA], 300, "Release 4.2 is cut. Please test your areas on staging today.");
    let replies = s.conversation(&account, ConversationRow {
        space: Some(ENGINEERING.to_owned()),
        parent: Some(general.id.clone()),
        root: Some(post.id.clone()),
        ..ConversationRow::new(format!("{}-replies", general.id), Thread, "Release 4.2 is cut")
    })?;
    s.say(&replies, &cast[BEN], 280, "Payments look fine.")?;
    s.say(&replies, &cast[DMITRI], 250, "Alerting dashboards updated.")?;
    post.thread = Some(ThreadSummary {
        id: replies.id.clone(),
        replies: 2,
        last: s.now - 250 * 60,
        participants: vec![Key::new(&account, cast[DMITRI].id.clone()), Key::new(&account, cast[BEN].id.clone())],
        unread: 1,
    });
    s.post(&general, &post)?;
    s.say(&general, &cast[ESI], 60, "Data export job moved to 02:00 UTC.")?;
    s.counts(&general, 1, 0, None)?;
    s.say(&Key::new(&account, engineering[1].id.clone()), &me, 55, "Rollout plan for 4.2 is attached.")?;
    s.say(&Key::new(&account, social[0].id.clone()), &cast[CHLOE], 2 * DAY, "Pottery class signup closes Friday.")?;
    Ok(me)
}
