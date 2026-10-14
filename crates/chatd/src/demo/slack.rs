//! The demo's Slack sign-in: two workspaces, so two accounts.

use super::png::Art;
use super::{Fallible, Listed, Seeder, Who};
use crate::store::{ConversationRow, SpaceRow};
use protocol::{
    AccountKey, CallSupport, ConversationKind, Group, GroupKind, Key, MediaKind, MentionKind,
    Network, Presence, Relation, Span, Style, ThreadSummary,
};
use Presence::{Dnd, Offline, Online};
use Relation::Contact;

pub const ME_HARBOR: &str = "U04ROBIN01";
pub const ME_FERNHILL: &str = "U07ROBIN02";

pub fn harbor() -> AccountKey {
    AccountKey {
        network: Network::Slack,
        account: "T04HARBOR1".to_owned(),
    }
}

pub fn fernhill() -> AccountKey {
    AccountKey {
        network: Network::Slack,
        account: "T07FERNHL2".to_owned(),
    }
}

/// A sidebar section and its channels as (name, unread, mentions, muted).
type Section = (&'static str, &'static [(&'static str, u32, u32, bool)]);
/// What is said in a channel or to a person: (speaker, minutes ago, text).
type Lines = &'static [(usize, i64, &'static str)];
/// A direct conversation: the cast member, unread, muted, and what was said.
type Direct = (usize, u32, bool, Lines);

const YOU: usize = usize::MAX;

struct Workspace {
    account: AccountKey,
    name: &'static str,
    me: &'static str,
    people: &'static [Listed],
    sections: &'static [Section],
    talk: &'static [(&'static str, Lines)],
    direct: &'static [Direct],
}

const HARBOR: &[Listed] = &[
    ("Mara Okafor", "mara", Online, "Reviewing the migration plan", Contact),
    ("Dev Patel", "dev", Online, "", Contact),
    ("Priya Raman", "priya", Dnd, "In planning until 2", Contact),
    ("Devon Reyes", "devon", Offline, "On leave until the 21st", Contact),
    ("Anneke Visser", "anneke", Online, "", Contact),
    ("Tunde Bakare", "tunde", Offline, "", Relation::None),
    ("Sofia Lindgren", "sofia", Online, "🌴 Working from Lisbon", Relation::None),
    ("Deploy Bot", "deploybot", Presence::Unknown, "", Relation::None),
];

const HARBOR_SECTIONS: &[Section] = &[
    ("Company", &[("general", 0, 0, false), ("announcements", 1, 0, false), ("random", 7, 0, true)]),
    ("Engineering", &[
        ("eng-platform", 4, 0, false),
        ("eng-incidents", 6, 2, false),
        ("eng-deploys", 20, 0, true),
        ("eng-reviews", 0, 0, false),
    ]),
    ("Product", &[("product-roadmap", 0, 0, false), ("design-crit", 2, 0, false), ("customer-voice", 3, 1, true)]),
    // Nothing said in a month and a half: Slack's "n quiet channels".
    ("Archive", &[("offsite-2025", 0, 0, false), ("old-launch", 0, 0, false)]),
];

const HARBOR_TALK: &[(&str, Lines)] = &[
    ("general", &[
        (4, 2800, "Office is closed Friday for the floor refit. Work from wherever has coffee."),
        (1, 2790, "So, the usual."),
        (6, 400, "Lisbon office says hello. It is 24 degrees and I am not sorry."),
    ]),
    ("announcements", &[
        (4, 6000, "Quarterly planning starts Monday. Bring one thing you want to stop doing."),
        (4, 180, "All-hands moved to Thursday 15:00. Recording will be posted here."),
    ]),
    ("eng-platform", &[
        (0, 1510, "Heads up: the migration for the events table is queued for tomorrow morning."),
        (YOU, 1502, "Sounds good. I will keep the consumers paused until it finishes."),
        (0, 41, "Migration finished clean. Resuming consumers now."),
        (0, 40, "Lag is back under a second."),
        (1, 12, "Canary looks healthy on my side, no new errors in the last 20 minutes."),
        (0, 3, "Deploy is green, rolling to prod at 3."),
    ]),
    ("eng-incidents", &[
        (5, 300, "Paging: checkout latency p99 is at 4 s in eu-west."),
        (1, 296, "Looking. It lines up with the cache node swap."),
        (5, 280, "@Robin Vale did the consumer pause go out to eu-west as well?"),
        (1, 270, "Rolled the node back. p99 is recovering."),
        (2, 262, "Customers saw about 9 minutes of slow checkouts. Drafting the note."),
        (5, 250, "@Robin Vale can you own the write-up? You have the timeline."),
        (1, 240, "Marking resolved. Follow-ups in the thread."),
    ]),
    ("eng-deploys", &[
        (7, 190, "api 4f2c1a9 deployed to staging by dev"),
        (7, 120, "api 4f2c1a9 deployed to production by mara"),
        (7, 30, "web 91be0d3 deployed to staging by anneke"),
    ]),
    ("design-crit", &[
        (4, 700, "New empty states are in the file. Be unkind."),
        (6, 90, "The illustration is carrying the whole screen. What does it say without it?"),
        (4, 85, "Less. Which is the problem, yes."),
    ]),
    ("customer-voice", &[
        (2, 900, "Three accounts asked for audit log export this week."),
        (2, 500, "@Robin Vale is export on the platform roadmap or ours?"),
        (3, 480, "Ours, last I heard."),
    ]),
    ("random", &[(1, 60, "Whoever left the oat milk out: it is a science project now.")]),
    ("offsite-2025", &[(4, 64_800, "Offsite photos are in the shared drive.")]),
    ("old-launch", &[(0, 70_000, "Launch retro notes, for the record.")]),
];

const HARBOR_DIRECT: &[Direct] = &[
    (0, 2, false, &[
        (0, 130, "Can you take the on-call handoff today?"),
        (YOU, 124, "On it."),
        (0, 20, "Thanks. Runbook link is in the channel topic."),
        (0, 19, "Also: the pager test at 4 is real this time."),
    ]),
    (1, 0, false, &[
        (1, 1300, "Review is up for the consumer pause change."),
        (YOU, 1290, "Approved with one nit about the timeout."),
        (1, 1285, "Fair nit. Fixed."),
    ]),
    (2, 3, true, &[
        (2, 700, "Planning doc is ready for comments."),
        (2, 650, "No rush."),
        (2, 640, "Some rush."),
    ]),
    (3, 0, false, &[(3, 9000, "Thanks for the review, merged. See you after the 21st.")]),
];

const FERNHILL: &[Listed] = &[
    ("Imogen Castellanos", "imogen", Online, "Coordinating the seed swap", Contact),
    ("Bartek Wolski", "bartek", Offline, "", Contact),
    ("Noor Haddad", "noor", Online, "", Relation::None),
];

const FERNHILL_SECTIONS: &[Section] = &[(
    "Channels",
    &[("general", 0, 0, false), ("events", 3, 0, false), ("volunteers", 2, 1, false), ("newsletter", 5, 0, true)],
)];

const FERNHILL_TALK: &[(&str, Lines)] = &[
    ("general", &[(0, 4000, "Welcome to the collective's new home. The old mailing list is retired.")]),
    ("events", &[
        (0, 800, "Seed swap is the 14th in the community hall."),
        (2, 300, "I can run the tomato table."),
        (1, 200, "Hall keys are with me until then."),
    ]),
    ("volunteers", &[
        (2, 500, "We need two more people for the Saturday watering rota."),
        (0, 110, "@Robin Vale could you take the 9:00 slot this week?"),
    ]),
    ("newsletter", &[(1, 1500, "Draft for the spring issue is in the shared folder.")]),
];

const FERNHILL_DIRECT: &[Direct] = &[
    (0, 1, false, &[
        (0, 600, "Do you still have the spare trestle tables?"),
        (YOU, 590, "Two, in the shed."),
        (0, 100, "Perfect. Can I collect them Thursday?"),
    ]),
    (1, 0, false, &[(YOU, 3000, "Left the hall keys in your letterbox."), (1, 2990, "Got them, thanks.")]),
];

/// What a channel is for, where its owners said, and whether only invited people see it.
fn about(channel: &str) -> (&'static str, bool) {
    match channel {
        "eng-platform" => ("Queues, consumers and the events pipeline. Runbook: go/platform", false),
        "eng-incidents" => ("One incident, one thread. Page first, post second.", false),
        "customer-voice" => ("What customers asked for this week", true),
        "volunteers" => ("Rotas and who has the keys", false),
        _ => ("", false),
    }
}

/// The id of a workspace's group of direct conversations.
fn directs(tag: char) -> String {
    format!("S0{tag}DIRECTS")
}

fn workspace(s: &mut Seeder, space: &Workspace, tag: char) -> Fallible<Who> {
    let account = &space.account;
    s.store.set_account(account, space.name, space.me)?;
    let me = Who {
        id: space.me.to_owned(),
        name: "Robin Vale".to_owned(),
        avatar: Some(s.avatar(account, &format!("avatar-{}", space.me), "Robin Vale")?),
        bot: false,
        me: true,
    };
    s.myself(account, &me)?;
    let cast = s.people(account, &format!("U0{tag}MEMBER"), space.people)?;

    let look = s.look(Art::Badge);
    let photo = if space.name == "Harbor Labs" { "logo-harbor-labs.png" } else { "icon-fernhill.webp" };
    let icon = s.photo(account, &format!("icon-{}", account.account), MediaKind::Icon, photo, look, 192, 192)?;
    // A workspace is the one Space of its account, under the team's own id.
    s.store.replace_spaces(account, &[SpaceRow {
        id: account.account.clone(),
        name: space.name.to_owned(),
        icon: Some(icon.id),
        position: 0,
    }])?;
    let mut groups = Vec::new();
    let mut rows = Vec::new();
    let mut state = Vec::new();
    for (name, channels) in space.sections {
        let group = format!("S0{tag}SECTION{}", groups.len());
        // The user set Product to show unread channels only.
        groups.push(Group { unread_only: *name == "Product", ..Group::new(group.clone(), *name, rows.len() as i32) });
        for (channel, unread, mentions, muted) in *channels {
            let id = format!("C0{tag}CHANNEL{:02}", rows.len());
            state.push((Key::new(account, id.clone()), *channel, *unread, *mentions, *muted));
            let (topic, private) = about(channel);
            rows.push(ConversationRow {
                group: Some(group.clone()),
                position: rows.len() as i32,
                call: CallSupport::Handoff,
                topic: topic.to_owned(),
                private,
                ..ConversationRow::new(id, ConversationKind::Channel, *channel)
            });
        }
    }
    // A workspace keeps its direct conversations inside it: they are rows of its tree, under
    // a group of their own, and still in the Inbox.
    groups.push(Group {
        kind: GroupKind::Directs,
        ..Group::new(directs(tag), "Direct messages", rows.len() as i32)
    });
    for (at, ..) in space.direct {
        let who = &cast[*at];
        rows.push(ConversationRow {
            group: Some(directs(tag)),
            position: rows.len() as i32,
            avatar: who.avatar.as_ref().map(|file| file.id.clone()),
            peer: Some(who.id.clone()),
            call: CallSupport::Handoff,
            ..ConversationRow::new(format!("D0{tag}DIRECT{at:03}"), ConversationKind::Direct, who.name.clone())
        });
    }
    s.store.replace_tree(&Key::new(account, account.account.clone()), &groups, &rows)?;

    for (conversation, channel, unread, mentions, muted) in &state {
        let lines = space.talk.iter().find(|(name, _)| name == channel).map_or(&[][..], |(_, lines)| *lines);
        let said = s.script(conversation, &cast, &me, lines)?;
        // The read marker goes above the last messages when the count allows it.
        let read = said.len().checked_sub(*unread as usize + 1).map(|at| said[at].as_str());
        s.counts(conversation, *unread, *mentions, read)?;
        s.store.set_muted(conversation, *muted)?;
    }

    for (at, unread, muted, lines) in space.direct {
        let conversation = Key::new(account, format!("D0{tag}DIRECT{at:03}"));
        let said = s.script(&conversation, &cast, &me, lines)?;
        let read = said.len().checked_sub(*unread as usize + 1).map(|at| said[at].as_str());
        s.counts(&conversation, *unread, 0, read)?;
        s.store.set_muted(&conversation, *muted)?;
    }
    Ok(me)
}

/// What only Harbor Labs has: a thread, a group conversation and a message to the channel.
fn harbor_extras(s: &mut Seeder) -> Fallible<()> {
    let account = harbor();
    let who = |id: usize, name: &str| Who {
        id: format!("U04MEMBER{:03}", id + 1),
        name: name.to_owned(),
        avatar: None,
        bot: false,
        me: false,
    };
    let (mara, dev) = (who(0, "Mara Okafor"), who(1, "Dev Patel"));

    let platform = Key::new(&account, "C04CHANNEL03");
    let mut root = s.message(&account, &mara, 1450, "Plan for the events migration is in the doc. Questions in this thread please.");
    let thread = s.conversation(&account, ConversationRow {
        space: Some(account.account.clone()),
        group: Some("S04SECTION1".to_owned()),
        // Between eng-platform and eng-incidents.
        position: 3,
        parent: Some(platform.id.clone()),
        root: Some(root.id.clone()),
        ..ConversationRow::new("C04CHANNEL03-thread", ConversationKind::Thread, "Thread in eng-platform")
    })?;
    s.say(&thread, &dev, 1400, "Do we need the consumers paused for the whole run?")?;
    s.say(&thread, &mara, 1395, "Only for the backfill step, about ten minutes.")?;
    s.say(&thread, &dev, 1390, "Then I will pause them from the runbook, not by hand.")?;
    root.thread = Some(ThreadSummary {
        id: thread.id.clone(),
        replies: 3,
        last: s.now - 1390 * 60,
        participants: vec![Key::new(&account, dev.id.clone()), Key::new(&account, mara.id.clone())],
        unread: 1,
    });
    s.post(&platform, &root)?;

    let text = "@channel the pager test at 16:00 is real. Acknowledge within five minutes. Details in #eng-incidents.";
    let mut everyone = s.message(&account, &mara, 2000, text);
    for (name, kind, id) in [("@channel", MentionKind::Everyone, ""), ("#eng-incidents", MentionKind::Channel, "C04CHANNEL04")] {
        let start = text.find(name).unwrap_or(0);
        everyone.spans.push(Span {
            start: start as u32,
            end: (start + name.len()) as u32,
            style: Style::Mention { kind, id: id.to_owned() },
        });
    }
    s.post(&platform, &everyone)?;
    // Older messages were added after the counts were set; they do not change them.

    let group = s.conversation(&account, ConversationRow {
        space: Some(account.account.clone()),
        group: Some(directs('4')),
        position: 99,
        call: CallSupport::Handoff,
        ..ConversationRow::new("G04GROUP001", ConversationKind::Group, "Mara Okafor, Dev Patel")
    })?;
    let said = [
        s.say(&group, &mara, 500, "Who is writing the incident note?")?,
        s.say(&group, &dev, 90, "Robin has the timeline, I have the graphs.")?,
        s.say(&group, &mara, 88, "Then Robin writes, Dev pastes, I edit.")?,
        s.say(&group, &dev, 80, "Democracy.")?,
    ];
    s.counts(&group, 3, 0, Some(&said[0]))
}

/// Seeds both workspaces and answers who we are in each.
pub fn seed(s: &mut Seeder) -> Fallible<Vec<(AccountKey, Who)>> {
    let harbor = Workspace {
        account: harbor(),
        name: "Harbor Labs",
        me: ME_HARBOR,
        people: HARBOR,
        sections: HARBOR_SECTIONS,
        talk: HARBOR_TALK,
        direct: HARBOR_DIRECT,
    };
    let mut me = vec![(harbor.account.clone(), workspace(s, &harbor, '4')?)];
    harbor_extras(s)?;
    let fernhill = Workspace {
        account: fernhill(),
        name: "Fernhill Collective",
        me: ME_FERNHILL,
        people: FERNHILL,
        sections: FERNHILL_SECTIONS,
        talk: FERNHILL_TALK,
        direct: FERNHILL_DIRECT,
    };
    me.push((fernhill.account.clone(), workspace(s, &fernhill, '7')?));
    Ok(me)
}
