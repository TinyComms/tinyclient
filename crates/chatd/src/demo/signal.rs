//! The demo's Signal account: a linked device's chat list, flat, with no Space, as Signal has
//! it. Only what the Signal adapter carries: direct and group chats, replies, a mention,
//! reactions and an edit. No presence, no calls and no pictures in messages.

use super::{Fallible, Listed, Seeder, Who};
use crate::store::ConversationRow;
use protocol::{AccountKey, ConversationKind, Key, Network, Presence, Reaction, Relation, ReplyTo};
use ConversationKind::{Direct, Group};
use Presence::Unknown;
use Relation::Contact;

/// Our ACI, which is also the account's id, as a linked device knows it.
pub const ME: &str = "5e1d7a20-93b4-4c8e-a6f1-2b0c9d4e7f31";
const CLIMBING: &str = "g.7c2f9a41d03e8b56";
const FLAT: &str = "g.e19b04c7a2d65f38";

pub fn account() -> AccountKey {
    AccountKey {
        network: Network::Signal,
        account: ME.to_owned(),
    }
}

/// Signal tells a linked device no presence and no status: every contact is just a contact.
const CONTACTS: &[Listed] = &[
    ("Theo Marchetti", "", Unknown, "", Contact),
    ("Sam Achterberg", "", Unknown, "", Contact),
    ("Leila Haddad", "", Unknown, "", Contact),
    ("Mara Okafor", "", Unknown, "", Contact),
    ("Lena Brandt", "", Unknown, "", Contact),
    ("Nonno Elio", "", Unknown, "", Contact),
];
const THEO: usize = 0;
const SAM: usize = 1;
const LEILA: usize = 2;
const MARA: usize = 3;
const LENA: usize = 4;
const ELIO: usize = 5;

fn reactions(list: &[(&str, u32, bool)]) -> Vec<Reaction> {
    list.iter().map(|(emoji, count, mine)| Reaction { emoji: (*emoji).to_owned(), file: None, count: *count, mine: *mine }).collect()
}

pub fn seed(s: &mut Seeder) -> Fallible<Who> {
    let account = account();
    s.store.set_account(&account, "Robin Vale", ME)?;
    let me = Who {
        id: ME.to_owned(),
        name: "Robin Vale".to_owned(),
        avatar: Some(s.avatar(&account, "signal-avatar-me", "Robin Vale")?),
        bot: false,
        me: true,
    };
    s.myself(&account, &me)?;
    let cast = s.people(&account, "signal-", CONTACTS)?;

    // Signal's own rows: listed, private, and a call is never offered.
    let direct = |who: &Who| ConversationRow {
        avatar: who.avatar.as_ref().map(|file| file.id.clone()),
        peer: Some(who.id.clone()),
        listed: true,
        private: true,
        call: protocol::CallSupport::None,
        ..ConversationRow::new(who.id.clone(), Direct, who.name.clone())
    };
    let group = |id: &str, title: &str, avatar: Option<String>| ConversationRow {
        avatar,
        listed: true,
        private: true,
        call: protocol::CallSupport::None,
        ..ConversationRow::new(id, Group, title)
    };
    let quote = |key: &Key, id: &str, who: &Who, text: &str| ReplyTo {
        id: id.to_owned(),
        sender: who.name.clone(),
        snippet: text.to_owned(),
        person: Some(Key::new(&key.account(), who.id.clone())),
        deleted: false,
    };

    // A walk being planned: a reply, a reaction of ours, two unread.
    let theo = s.conversation(&account, direct(&cast[THEO]))?;
    let said = s.script(&theo, &cast, &me, &[
        (THEO, 190, "Are you still up for the coast walk on Sunday?"),
        (usize::MAX, 182, "Yes! Leaving early?"),
    ])?;
    let mut coffee = s.message(&account, &cast[THEO], 34, "8:00 from the station. I'll bring the coffee.");
    coffee.reactions = reactions(&[("❤️", 1, true)]);
    s.post(&theo, &coffee)?;
    let mut early = s.message(&account, &cast[THEO], 9, "Early enough to beat the crowds 😄");
    early.reply_to = Some(quote(&theo, &said[1], &me, "Yes! Leaving early?"));
    s.post(&theo, &early)?;
    s.say(&theo, &cast[THEO], 8, "Low tide is at 10:40, so we can do the long beach.")?;
    s.counts(&theo, 2, 0, Some(&said[1]))?;

    // A group with a mention of us, reactions and an edited message.
    let avatar = s.cover(&account, CLIMBING, "group-climbing-sundays.webp")?;
    let climbing = s.conversation(&account, group(CLIMBING, "Climbing Sundays", avatar))?;
    let said = s.script(&climbing, &cast, &me, &[(SAM, 420, "Gym or crag this week?")])?;
    let mut dry = s.message(&account, &cast[LEILA], 400, "Crag, if the rock is dry by Sunday. Forecast looks good.");
    dry.edited = true;
    dry.reactions = reactions(&[("👍", 3, true)]);
    s.post(&climbing, &dry)?;
    s.say(&climbing, &cast[LENA], 120, "I can drive. Four seats.")?;
    let mut rope = s.message(&account, &cast[SAM], 26, "@Robin Vale you're on rope duty 🧗");
    rope.reply_to = Some(quote(&climbing, &said[0], &cast[SAM], "Gym or crag this week?"));
    s.post(&climbing, &rope)?;
    let mut packed = s.message(&account, &me, 21, "Rope is packed. And the spare harness.");
    packed.reactions = reactions(&[("🙌", 2, false), ("🔥", 1, false)]);
    s.post(&climbing, &packed)?;
    s.say(&climbing, &cast[LEILA], 4, "Meet at the car park at 9?")?;
    s.counts(&climbing, 1, 0, Some(&packed.id))?;

    // Read, ours last.
    let mara = s.conversation(&account, direct(&cast[MARA]))?;
    let said = s.script(&mara, &cast, &me, &[
        (MARA, 300, "Dinner Thursday?"),
        (usize::MAX, 290, "Thursday works. 7?"),
    ])?;
    let mut seven = s.message(&account, &cast[MARA], 285, "7 it is. I'll book the place by the river.");
    seven.reactions = reactions(&[("👍", 1, true)]);
    s.post(&mara, &seven)?;
    s.counts(&mara, 0, 0, Some(&said[1]))?;

    let avatar = s.cover(&account, FLAT, "group-flat-3b.webp")?;
    let flat = s.conversation(&account, group(FLAT, "Flat 3B", avatar))?;
    let said = s.script(&flat, &cast, &me, &[
        (LEILA, 900, "Who's home for the delivery tomorrow?"),
        (usize::MAX, 880, "Me, from noon."),
        (LENA, 870, "Thank you!! Leaving a key under the plant."),
    ])?;
    s.counts(&flat, 0, 0, Some(&said[2]))?;

    let elio = s.conversation(&account, direct(&cast[ELIO]))?;
    let said = s.script(&elio, &cast, &me, &[
        (ELIO, 2900, "Ciao Robin! Did the photos of the garden arrive?"),
        (usize::MAX, 2880, "They did. The tomatoes look amazing."),
        (ELIO, 2870, "Next year even bigger 🍅"),
    ])?;
    s.counts(&elio, 0, 0, Some(&said[2]))?;
    Ok(me)
}
