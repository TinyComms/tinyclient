//! First-run setup on a Tiny server, as plain values: who is shown it, which options are picked,
//! what each way of finishing sends, what each refusal says, and what everyone who is not an owner
//! is told the server keeps. The window draws these; they are tested without one.
//!
//! The server writes the question and the effect of every choice and option (`GET /v1/setup`, for
//! owners), and this client draws that text as it comes, so the two never drift. The one place
//! this file writes such words itself is [`keeps`]: a member who is not an owner cannot ask for the
//! server's outline, and still may read what it keeps about them.

use protocol::{TinyCommunity, TinyPosture, TinyServerInfo, TinySetup, TinySetupApplied, TinySetupApply, TinySetupRefusal, TINY_FEATURE_FIRST_RUN_SETUP};

/// The server has setup. An older server, a bot's session and every other network never do, and
/// nothing about setup is drawn for them.
pub fn offered(community: &TinyCommunity) -> bool {
    community.has(TINY_FEATURE_FIRST_RUN_SETUP)
}

/// This account may read the outline and change the choices: the server has setup and `GET /v1/me`
/// says the account is an owner. An admin with every permission is not: these choices say what
/// the community keeps about everyone in it.
pub fn may_choose(community: &TinyCommunity) -> bool {
    offered(community) && community.access.as_ref().is_some_and(|access| access.owner)
}

/// What the owner is shown when the server page opens: the first-run screen, once the server says
/// setup is needed and this open of the page has not already been put off with "Later".
pub fn asks_now(community: &TinyCommunity, setup: &TinySetup, put_off: bool) -> bool {
    may_choose(community) && setup.needed && !put_off
}

/// What was read of setup no longer says what the server says. The server page is read again
/// whenever the live socket reports a change (`server.updated` when an owner applies setup,
/// here or on another device), and its three values and the time of the answer are compared with
/// the outline held. A server that said none of them (an older one) is never stale.
pub fn stale(setup: &TinySetup, server: Option<&TinyServerInfo>) -> bool {
    let Some(server) = server else {
        return false;
    };
    let said = server.posture();
    let says_choices = !said.provider_profile.is_empty() || !said.activity_visibility.is_empty() || !said.sign_in_methods.is_empty();
    (says_choices && said != setup.current) || (setup.needed && !server.setup_completed_at.is_empty())
}

/// What the window holds of setup, as far as deciding what to do next goes.
pub struct Held<'a> {
    /// The outline as last read.
    pub data: Option<&'a TinySetup>,
    /// A read or an answer is with the server.
    pub busy: bool,
    /// The first-run screen is up.
    pub prompt: bool,
    /// "Later" was chosen since the server was opened.
    pub put_off: bool,
}

/// What to do once the server page has been read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Next {
    Nothing,
    /// Let go of all of it: this account is no longer an owner of a server with setup.
    Forget,
    /// Ask the server for the outline; with `prompt`, show the first-run screen if it says setup is
    /// needed.
    Read { prompt: bool },
    /// The outline is held, setup is still needed and was not put off: show the first-run screen.
    Prompt,
}

/// The first-run screen comes up when an owner opens a server that still needs setup, once, and
/// again each time the server is opened until they answer; "Later" puts it off until then. The
/// outline is read when none is held, and again when the server says something else than what
/// is held (another device answered), which also takes the screen away.
pub fn after_page_read(community: &TinyCommunity, held: &Held) -> Next {
    if !may_choose(community) {
        return if held.data.is_some() || held.prompt { Next::Forget } else { Next::Nothing };
    }
    if held.busy {
        return Next::Nothing;
    }
    if held.data.is_none_or(|data| stale(data, community.server.as_ref())) {
        return Next::Read { prompt: !held.put_off };
    }
    if !held.prompt && held.data.is_some_and(|data| asks_now(community, data, held.put_off)) {
        return Next::Prompt;
    }
    Next::Nothing
}

/// Which preset, option and flags the form holds, from which the answer is made.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Form {
    /// What each choice is set to in the form.
    pub picked: TinyPosture,
    /// The preset last picked. It goes in the answer, and what differs from it goes beside it.
    pub preset: Option<String>,
    /// Remove the email address of every member. Off until the owner turns it on.
    pub erase_emails: bool,
    /// Go on although members have no passkey yet. Only set after the server refused for it.
    pub allow_lockout: bool,
}

impl Form {
    /// The form as it starts: every choice as the server has it now, no preset picked.
    pub fn starting(setup: &TinySetup) -> Form {
        Form { picked: setup.current.clone(), ..Form::default() }
    }

    /// Picks a preset: all three choices become what it sets. One this server cannot do (or one it
    /// does not list) changes nothing, and `false` says so.
    pub fn pick_preset(&mut self, setup: &TinySetup, id: &str) -> bool {
        let Some(preset) = setup.presets.iter().find(|preset| preset.id == id && preset.available) else {
            return false;
        };
        self.picked = preset.choices.clone();
        self.preset = Some(preset.id.clone());
        // Permission to lock people out was for the choice that was refused, not for this one.
        self.allow_lockout = false;
        true
    }

    /// Picks one option of one choice, on top of whatever was picked before, a preset included.
    /// One this server cannot do changes nothing, and `false` says so.
    pub fn pick_option(&mut self, setup: &TinySetup, choice: &str, option: &str) -> bool {
        let listed = setup.choices.iter().find(|listed| listed.id == choice).and_then(|listed| listed.options.iter().find(|listed| listed.id == option));
        let Some(listed) = listed else {
            return false;
        };
        if blocked(setup, &listed.needs).is_some() {
            return false;
        }
        self.allow_lockout = false;
        self.picked.set(choice, option)
    }

    pub fn is_picked(&self, choice: &str, option: &str) -> bool {
        self.picked.value(choice) == Some(option)
    }

    /// Something in the form differs from what the server keeps now: a choice, or the email
    /// addresses to be erased. A preset that sets what is already set is not a change.
    pub fn changed(&self, setup: &TinySetup) -> bool {
        self.picked != setup.current || self.erases(setup)
    }

    /// The form can be applied as it stands: a preset was picked, or something differs.
    pub fn answers(&self, setup: &TinySetup) -> bool {
        self.preset.is_some() || self.changed(setup)
    }

    /// Server Settings has something to save: a change, or, while setup is still unanswered, a
    /// preset, even one that sets what is already set (that is the owner's answer).
    pub fn unsaved(&self, setup: &TinySetup) -> bool {
        self.changed(setup) || (setup.needed && self.preset.is_some())
    }

    /// The addresses will really be erased: asked for, and there are some.
    pub fn erases(&self, setup: &TinySetup) -> bool {
        self.erase_emails && setup.counts.emails_stored > 0
    }

    /// What `PUT /v1/setup` is sent. A preset goes as itself, then each choice that differs from
    /// what the preset sets; with no preset, each choice that differs from what the server has.
    pub fn apply(&self, setup: &TinySetup) -> TinySetupApply {
        let preset = self.preset.as_ref().and_then(|id| setup.presets.iter().find(|preset| preset.id == *id));
        let base = preset.map_or(&setup.current, |preset| &preset.choices);
        let differs = |choice: &str| {
            let now = self.picked.value(choice)?;
            (base.value(choice) != Some(now)).then(|| now.to_owned())
        };
        TinySetupApply {
            preset: preset.map(|preset| preset.id.clone()),
            provider_profile: differs("provider_profile"),
            activity_visibility: differs("activity_visibility"),
            sign_in_methods: differs("sign_in_methods"),
            erase_emails: self.erases(setup),
            allow_lockout: self.allow_lockout,
        }
    }
}

/// "Keep what I have": nothing is changed, setup is done. An empty answer, which goes as `{}`.
pub fn keep() -> TinySetupApply {
    TinySetupApply::default()
}

/// What an option depends on that this server lacks: `None` when it can be chosen. Only a missing
/// feature of the deployment blocks it; an owner without a passkey yet is told so ([`hint`]) and
/// the server decides.
pub fn blocked(setup: &TinySetup, needs: &[String]) -> Option<&'static str> {
    (needs.iter().any(|need| need == "passkeys") && !setup.deployment.passkeys).then_some("This server was started without passkey sign-in.")
}

/// Something to say beside an option the owner has picked, before they commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hint {
    /// They have no passkey of their own, and the option needs one.
    AddPasskey,
}

pub fn hint(setup: &TinySetup, needs: &[String]) -> Option<Hint> {
    (needs.iter().any(|need| need == "owner_has_passkey") && !setup.counts.owner_has_passkey).then_some(Hint::AddPasskey)
}

pub const ADD_PASSKEY_WORDS: &str = "You don't have a passkey of your own yet. Add one first, or you could not sign in again.";

/// What the owner may do about a refusal, as a button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Remedy {
    /// Ask the server for a link that adds a passkey for this account, and open it.
    AddPasskey,
    /// Send the same answer again with `allow_lockout`.
    DoItAnyway,
}

pub fn remedy(refusal: &TinySetupRefusal) -> Option<Remedy> {
    match refusal {
        TinySetupRefusal::PasskeysUnavailable => None,
        TinySetupRefusal::OwnerNeedsPasskey => Some(Remedy::AddPasskey),
        TinySetupRefusal::MembersWithoutPasskey { .. } => Some(Remedy::DoItAnyway),
    }
}

pub fn remedy_label(remedy: Remedy) -> &'static str {
    match remedy {
        Remedy::AddPasskey => "Add a passkey for me",
        Remedy::DoItAnyway => "Do it anyway",
    }
}

/// What a refusal says: what is in the way, and what can be done.
pub fn refusal_words(refusal: &TinySetupRefusal) -> String {
    match refusal {
        TinySetupRefusal::PasskeysUnavailable => {
            "This server was started without passkey sign-in, so members can't be limited to passkeys. Choose another way to sign in, or ask whoever runs the server to turn passkeys on.".to_owned()
        }
        TinySetupRefusal::OwnerNeedsPasskey => {
            "You don't have a passkey of your own yet. If members were limited to passkeys now, you could not sign in again. Add a passkey for yourself first, then choose this again.".to_owned()
        }
        TinySetupRefusal::MembersWithoutPasskey { count } => format!(
            "{} no passkey yet. If you go on, they can't sign in until you give each of them a passkey link. Anyone already signed in stays signed in until their session ends.",
            people_without(*count)
        ),
    }
}

/// `1 member has`, `3 members have`.
fn people_without(count: u32) -> String {
    match count {
        1 => "1 member has".to_owned(),
        n => format!("{n} members have"),
    }
}

/// The counts that matter before the owner commits, as lines of words. Each is left out when it
/// has nothing to say, so a server with no passkeys and no addresses says nothing here.
pub fn counts_words(setup: &TinySetup) -> Vec<String> {
    let counts = &setup.counts;
    let mut lines = Vec::new();
    if setup.deployment.passkeys {
        lines.push(match counts.members_without_passkey {
            0 => "Every member has a passkey.".to_owned(),
            n => format!("{} no passkey yet.", people_without(n)),
        });
    }
    match counts.emails_stored {
        0 => {}
        1 => lines.push("1 email address is stored.".to_owned()),
        n => lines.push(format!("{n} email addresses are stored.")),
    }
    lines
}

pub const ERASE_LABEL: &str = "Also remove the email addresses already on file";
pub const ERASE_WARNING: &str = "This removes the email address of every member, yours too. It cannot be undone.";

/// What was done beyond the choices, for the line shown once the answer went through.
pub fn applied_words(applied: &TinySetupApplied) -> String {
    let mut words = vec!["Saved.".to_owned()];
    match applied.emails_erased {
        0 => {}
        1 => words.push("1 email address was removed.".to_owned()),
        n => words.push(format!("{n} email addresses were removed.")),
    }
    match applied.last_seen_cleared {
        0 => {}
        1 => words.push("1 last-online time was cleared.".to_owned()),
        n => words.push(format!("{n} last-online times were cleared.")),
    }
    words.join(" ")
}

/// What the server keeps now, in its own words: each choice's title, with the title and effect of
/// the option that is current. A choice whose current value the outline does not list is left out.
pub fn right_now(setup: &TinySetup) -> Vec<(String, String, String)> {
    setup
        .choices
        .iter()
        .filter_map(|choice| {
            let current = setup.current.value(&choice.id)?;
            let option = choice.options.iter().find(|option| option.id == current)?;
            Some((choice.title.clone(), option.title.clone(), option.effect.clone()))
        })
        .collect()
}

/// One line of what the server keeps, for someone who cannot ask the server for its outline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Keeps {
    pub heading: &'static str,
    pub words: &'static str,
}

const UNKNOWN: &str = "This server set it in a way this version of tinyclient does not know. Update tinyclient to read it.";

pub fn provider_words(value: &str) -> &'static str {
    match value {
        "full" => "When someone first joins through single sign-on, the server keeps their name and email address from the provider. People who manage the server can read members' email addresses.",
        "name" => "When someone first joins through single sign-on, the server keeps their name and never an email address.",
        "none" => "The server keeps nothing from a sign-in provider. New members start as \u{201c}Member\u{201d} and choose their own name.",
        _ => UNKNOWN,
    }
}

pub fn activity_words(value: &str) -> &'static str {
    match value {
        "everyone" => "Every member can see when others were last online and how they joined.",
        "admins" => "Only people who manage the server can see when members were last online and how they joined.",
        "nobody" => "The server does not record when anyone was last online, and no one sees how a member joined.",
        _ => UNKNOWN,
    }
}

pub fn sign_in_words(value: &str) -> &'static str {
    match value {
        "any" => "Members can sign in with any method this server offers.",
        "passkey" => "Members sign in with a passkey and nothing else. A single sign-on account alone does not get in.",
        _ => UNKNOWN,
    }
}

/// What the server keeps, by the three values `GET /v1/server` gives every member. A value the
/// server did not send (an older server) is left out rather than guessed.
pub fn keeps(server: &TinyServerInfo) -> Vec<Keeps> {
    let mut lines = Vec::new();
    if !server.provider_profile.is_empty() {
        lines.push(Keeps { heading: "What it keeps from a sign-in provider", words: provider_words(&server.provider_profile) });
    }
    if !server.activity_visibility.is_empty() {
        lines.push(Keeps { heading: "Who sees when members were last online", words: activity_words(&server.activity_visibility) });
    }
    if !server.sign_in_methods.is_empty() {
        lines.push(Keeps { heading: "How members sign in", words: sign_in_words(&server.sign_in_methods) });
    }
    lines
}

/// Whether the owner has made these choices, in words. Until they do, the server keeps what it
/// always did: a community that was running is not changed by an upgrade.
pub fn decided_words(server: &TinyServerInfo) -> &'static str {
    if server.setup_completed_at.is_empty() {
        "The owner hasn't chosen yet, so the server keeps what it always has."
    } else {
        "The owner chose these."
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::{
        TinyAccess, TinyCapabilities, TinyChoice, TinyDeployment, TinyOption, TinyPreset, TinySetupCounts, TINY_ACTIVITY_VISIBILITIES, TINY_PROVIDER_PROFILES, TINY_SIGN_IN_METHODS,
    };

    fn option(id: &str, needs: &[&str]) -> TinyOption {
        TinyOption { id: id.into(), title: id.into(), effect: format!("{id} does this."), needs: needs.iter().map(|need| (*need).to_owned()).collect() }
    }

    fn posture(profile: &str, activity: &str, sign_in: &str) -> TinyPosture {
        TinyPosture { provider_profile: profile.into(), activity_visibility: activity.into(), sign_in_methods: sign_in.into() }
    }

    /// The outline the server describes in `docs/server-setup.md`, on a server with passkeys and
    /// single sign-on, one member without a passkey, two addresses stored, and an owner with none.
    fn setup() -> TinySetup {
        TinySetup {
            needed: true,
            completed_at: String::new(),
            current: posture("full", "everyone", "any"),
            deployment: TinyDeployment { passkeys: true, sso: true },
            counts: TinySetupCounts { members: 3, members_without_passkey: 2, emails_stored: 2, owner_has_passkey: false },
            presets: vec![
                TinyPreset { id: "convenience".into(), title: "Convenience".into(), summary: "Easy.".into(), choices: posture("full", "everyone", "any"), available: true, unavailable: String::new() },
                TinyPreset { id: "privacy".into(), title: "Privacy".into(), summary: "Least.".into(), choices: posture("none", "nobody", "passkey"), available: true, unavailable: String::new() },
            ],
            choices: vec![
                TinyChoice {
                    id: "provider_profile".into(),
                    title: "Provider".into(),
                    question: "What to keep?".into(),
                    relevant: true,
                    options: vec![option("full", &[]), option("name", &[]), option("none", &[])],
                },
                TinyChoice {
                    id: "activity_visibility".into(),
                    title: "Activity".into(),
                    question: "Who sees?".into(),
                    relevant: true,
                    options: vec![option("everyone", &[]), option("admins", &[]), option("nobody", &[])],
                },
                TinyChoice {
                    id: "sign_in_methods".into(),
                    title: "Sign-in".into(),
                    question: "Which?".into(),
                    relevant: true,
                    options: vec![option("any", &[]), option("passkey", &["passkeys", "owner_has_passkey"])],
                },
            ],
            always: vec!["There are no usernames.".into()],
        }
    }

    /// What `PUT /v1/setup` is sent. Its JSON (`{}` for nothing, only what is set otherwise) is
    /// the protocol crate's to test.
    fn answer(preset: Option<&str>, choices: [Option<&str>; 3], erase_emails: bool, allow_lockout: bool) -> TinySetupApply {
        let [provider_profile, activity_visibility, sign_in_methods] = choices.map(|choice| choice.map(str::to_owned));
        TinySetupApply { preset: preset.map(str::to_owned), provider_profile, activity_visibility, sign_in_methods, erase_emails, allow_lockout }
    }

    fn community(features: &[&str], owner: bool) -> TinyCommunity {
        TinyCommunity {
            role: "member".into(),
            members: Vec::new(),
            invites: Vec::new(),
            audit: Vec::new(),
            origin: String::new(),
            rooms: Vec::new(),
            online: Vec::new(),
            media: String::new(),
            capabilities: TinyCapabilities { version: String::new(), features: features.iter().map(|feature| (*feature).to_owned()).collect() },
            access: Some(TinyAccess { owner, permissions: protocol::TINY_ALL, ..TinyAccess::default() }),
            me: String::new(),
            server: None,
            roles: Vec::new(),
            categories: Vec::new(),
            bans: Vec::new(),
            provider: String::new(),
        }
    }

    #[test]
    fn only_an_owner_of_a_server_with_setup_is_asked() {
        let owner = community(&["server", "first_run_setup"], true);
        assert!(offered(&owner) && may_choose(&owner));
        assert!(asks_now(&owner, &setup(), false));
        assert!(!asks_now(&owner, &setup(), true), "put off with Later: not again until the server is opened again");
        assert!(!asks_now(&owner, &TinySetup { needed: false, ..setup() }, false), "already answered");
        let admin = community(&["server", "first_run_setup"], false);
        assert!(offered(&admin) && !may_choose(&admin), "an admin with every permission is not an owner here");
        assert!(!asks_now(&admin, &setup(), false));
        let older = community(&["server", "roles"], true);
        assert!(!offered(&older) && !may_choose(&older), "a server without the feature shows nothing");
        let mut legacy = community(&["first_run_setup"], true);
        legacy.access = None;
        legacy.role = "owner".into();
        assert!(!may_choose(&legacy), "the saved legacy role is not what GET /v1/me says");
    }

    #[test]
    fn a_form_starts_from_what_the_server_has_and_a_preset_replaces_all_three() {
        let setup = setup();
        let mut form = Form::starting(&setup);
        assert_eq!(form.picked, setup.current);
        assert!(!form.changed(&setup) && !form.answers(&setup), "nothing picked yet");
        assert!(form.pick_preset(&setup, "privacy"));
        assert_eq!(form.picked, posture("none", "nobody", "passkey"));
        assert!(form.changed(&setup) && form.answers(&setup));
        assert!(form.is_picked("sign_in_methods", "passkey") && !form.is_picked("sign_in_methods", "any"));
        assert!(!form.pick_preset(&setup, "nope"), "a preset the server does not list is not picked");
        assert_eq!(form.preset.as_deref(), Some("privacy"));
        // Convenience sets what is already set: it can be applied, but it is not a change.
        let mut same = Form::starting(&setup);
        assert!(same.pick_preset(&setup, "convenience"));
        assert!(!same.changed(&setup) && same.answers(&setup));
        // On Server Settings that is something to save only while setup is unanswered.
        assert!(same.unsaved(&setup));
        let answered = TinySetup { needed: false, ..setup.clone() };
        assert!(!same.unsaved(&answered), "nothing to save once answered");
        assert!(form.unsaved(&answered), "a change always is");
        assert!(!Form::starting(&setup).unsaved(&setup));
    }

    #[test]
    fn the_answer_for_each_way_of_finishing() {
        let setup = setup();
        // Keep what I have is a complete answer and sends nothing.
        assert_eq!(keep(), answer(None, [None, None, None], false, false));
        // A preset alone.
        let mut form = Form::starting(&setup);
        form.pick_preset(&setup, "privacy");
        assert_eq!(form.apply(&setup), answer(Some("privacy"), [None, None, None], false, false));
        // A preset, and one choice changed back: only that choice goes beside it.
        assert!(form.pick_option(&setup, "sign_in_methods", "any"));
        assert_eq!(form.apply(&setup), answer(Some("privacy"), [None, None, Some("any")], false, false));
        // No preset: each choice that differs from what the server has, and no more.
        let mut custom = Form::starting(&setup);
        assert!(custom.pick_option(&setup, "activity_visibility", "admins"));
        assert_eq!(custom.apply(&setup), answer(None, [None, Some("admins"), None], false, false));
        assert!(custom.pick_option(&setup, "provider_profile", "name"));
        assert_eq!(custom.apply(&setup), answer(None, [Some("name"), Some("admins"), None], false, false));
        // Picking the value the server already has again is no change.
        assert!(custom.pick_option(&setup, "provider_profile", "full") && custom.pick_option(&setup, "activity_visibility", "everyone"));
        assert_eq!(custom.apply(&setup), keep());
        assert!(!custom.changed(&setup));
    }

    #[test]
    fn erasing_email_addresses_is_off_until_asked_and_only_where_there_are_some() {
        let mut setup = setup();
        let mut form = Form::starting(&setup);
        form.pick_preset(&setup, "privacy");
        assert!(!form.erase_emails, "off by default");
        assert!(!form.apply(&setup).erase_emails);
        form.erase_emails = true;
        assert_eq!(form.apply(&setup), answer(Some("privacy"), [None, None, None], true, false));
        assert!(form.changed(&setup));
        // Nothing on file: nothing to remove, and the flag is not sent.
        setup.counts.emails_stored = 0;
        assert_eq!(form.apply(&setup), answer(Some("privacy"), [None, None, None], false, false));
        assert!(!form.erases(&setup));
        // Erasing on its own is a change the page can save.
        let mut only = Form::starting(&TinySetup { counts: TinySetupCounts { emails_stored: 4, ..setup.counts.clone() }, ..setup.clone() });
        only.erase_emails = true;
        let with_emails = TinySetup { counts: TinySetupCounts { emails_stored: 4, ..setup.counts.clone() }, ..setup.clone() };
        assert!(only.changed(&with_emails));
        assert_eq!(only.apply(&with_emails), answer(None, [None, None, None], true, false));
    }

    #[test]
    fn allowing_a_lockout_goes_with_the_answer_and_is_forgotten_when_the_choice_changes() {
        let setup = setup();
        let mut form = Form::starting(&setup);
        form.pick_preset(&setup, "privacy");
        form.allow_lockout = true;
        assert_eq!(form.apply(&setup), answer(Some("privacy"), [None, None, None], false, true));
        assert!(form.pick_option(&setup, "sign_in_methods", "any"));
        assert!(!form.allow_lockout, "permission was for the choice that was refused");
        form.allow_lockout = true;
        form.pick_preset(&setup, "convenience");
        assert!(!form.allow_lockout);
        assert!(!form.apply(&setup).allow_lockout);
    }

    #[test]
    fn a_preset_or_option_the_server_cannot_do_is_not_picked() {
        let mut setup = setup();
        setup.deployment.passkeys = false;
        setup.presets[1].available = false;
        setup.presets[1].unavailable = "This server was started without passkey sign-in.".into();
        let mut form = Form::starting(&setup);
        assert!(!form.pick_preset(&setup, "privacy"));
        assert_eq!((form.preset.clone(), form.picked.clone()), (None, setup.current.clone()));
        assert!(!form.pick_option(&setup, "sign_in_methods", "passkey"), "needs passkeys, which this server does not have");
        assert!(form.pick_option(&setup, "sign_in_methods", "any"));
        assert!(!form.pick_option(&setup, "sign_in_methods", "nothing"), "an option the server does not list");
        assert!(!form.pick_option(&setup, "mystery", "x"), "a choice the server does not list");
        assert_eq!(blocked(&setup, &["passkeys".into()]), Some("This server was started without passkey sign-in."));
        assert_eq!(blocked(&setup, &[]), None);
        // Privacy with the sign-in choice left alone, as the server's own text suggests, is the
        // owner's: a preset that is unavailable, then each of its other two choices by hand.
        assert!(form.pick_option(&setup, "provider_profile", "none") && form.pick_option(&setup, "activity_visibility", "nobody"));
        assert_eq!(form.apply(&setup), answer(None, [Some("none"), Some("nobody"), None], false, false));
    }

    #[test]
    fn a_passkey_only_option_tells_an_owner_without_a_passkey_so_before_they_commit() {
        let mut setup = setup();
        let needs = vec!["passkeys".to_owned(), "owner_has_passkey".to_owned()];
        assert_eq!(hint(&setup, &needs), Some(Hint::AddPasskey));
        assert_eq!(hint(&setup, &[]), None);
        setup.counts.owner_has_passkey = true;
        assert_eq!(hint(&setup, &needs), None);
    }

    #[test]
    fn each_refusal_says_what_is_in_the_way_and_offers_what_can_be_done() {
        let owner = refusal_words(&TinySetupRefusal::OwnerNeedsPasskey);
        assert!(owner.contains("passkey of your own") && owner.contains("Add a passkey for yourself first"), "{owner}");
        assert_eq!(remedy(&TinySetupRefusal::OwnerNeedsPasskey), Some(Remedy::AddPasskey));
        let members = refusal_words(&TinySetupRefusal::MembersWithoutPasskey { count: 3 });
        assert!(members.starts_with("3 members have no passkey yet.") && members.contains("can't sign in until you give each of them a passkey link"), "{members}");
        assert!(refusal_words(&TinySetupRefusal::MembersWithoutPasskey { count: 1 }).starts_with("1 member has no passkey yet."));
        assert_eq!(remedy(&TinySetupRefusal::MembersWithoutPasskey { count: 3 }), Some(Remedy::DoItAnyway));
        let off = refusal_words(&TinySetupRefusal::PasskeysUnavailable);
        assert!(off.contains("started without passkey sign-in"), "{off}");
        assert_eq!(remedy(&TinySetupRefusal::PasskeysUnavailable), None, "nothing the owner can press fixes a server without passkeys");
        assert_eq!((remedy_label(Remedy::AddPasskey), remedy_label(Remedy::DoItAnyway)), ("Add a passkey for me", "Do it anyway"));
    }

    #[test]
    fn the_counts_that_matter_are_said_before_the_owner_commits() {
        let mut setup = setup();
        assert_eq!(counts_words(&setup), ["2 members have no passkey yet.", "2 email addresses are stored."]);
        setup.counts = TinySetupCounts { members: 1, members_without_passkey: 1, emails_stored: 1, owner_has_passkey: false };
        assert_eq!(counts_words(&setup), ["1 member has no passkey yet.", "1 email address is stored."]);
        setup.counts = TinySetupCounts { members: 4, members_without_passkey: 0, emails_stored: 0, owner_has_passkey: true };
        assert_eq!(counts_words(&setup), ["Every member has a passkey."]);
        setup.deployment.passkeys = false;
        assert!(counts_words(&setup).is_empty(), "no passkeys here: nothing to count");
    }

    #[test]
    fn what_applying_did_beyond_the_choices_is_said() {
        let applied = |emails, seen| TinySetupApplied { emails_erased: emails, last_seen_cleared: seen, ..TinySetupApplied::default() };
        assert_eq!(applied_words(&applied(0, 0)), "Saved.");
        assert_eq!(applied_words(&applied(1, 1)), "Saved. 1 email address was removed. 1 last-online time was cleared.");
        assert_eq!(applied_words(&applied(3, 0)), "Saved. 3 email addresses were removed.");
        assert_eq!(applied_words(&applied(0, 5)), "Saved. 5 last-online times were cleared.");
    }

    /// Everyone who is not an owner is told what the server keeps from these three values, so
    /// every value the server can send needs words, and one it cannot yet send still gets some.
    #[test]
    fn every_value_the_server_can_send_has_words_for_a_member() {
        let all_distinct = |words: Vec<&str>| {
            assert!(words.iter().all(|words| !words.is_empty() && *words != UNKNOWN), "{words:?}");
            let mut sorted = words.clone();
            sorted.sort_unstable();
            sorted.dedup();
            assert_eq!(sorted.len(), words.len(), "two values read the same: {words:?}");
        };
        all_distinct(TINY_PROVIDER_PROFILES.iter().map(|value| provider_words(value)).collect());
        all_distinct(TINY_ACTIVITY_VISIBILITIES.iter().map(|value| activity_words(value)).collect());
        all_distinct(TINY_SIGN_IN_METHODS.iter().map(|value| sign_in_words(value)).collect());
        assert_eq!((provider_words("someday"), activity_words("someday"), sign_in_words("someday")), (UNKNOWN, UNKNOWN, UNKNOWN));
    }

    #[test]
    fn a_member_reads_the_three_values_and_an_older_server_says_nothing() {
        let mut server = TinyServerInfo { provider_profile: "none".into(), activity_visibility: "nobody".into(), sign_in_methods: "passkey".into(), ..TinyServerInfo::default() };
        let lines = keeps(&server);
        assert_eq!(lines.iter().map(|line| line.heading).collect::<Vec<_>>(), ["What it keeps from a sign-in provider", "Who sees when members were last online", "How members sign in"]);
        assert_eq!(lines[0].words, provider_words("none"));
        assert_eq!(decided_words(&server), "The owner hasn't chosen yet, so the server keeps what it always has.");
        server.setup_completed_at = "2026-11-01T10:00:00Z".into();
        assert_eq!(decided_words(&server), "The owner chose these.");
        assert!(keeps(&TinyServerInfo::default()).is_empty(), "a server that sent none of the three: nothing is guessed");
        server.activity_visibility.clear();
        assert_eq!(keeps(&server).len(), 2);
    }

    #[test]
    fn the_first_run_screen_comes_once_per_opening_and_only_for_an_owner() {
        let owner = community(&["first_run_setup"], true);
        let needed = setup();
        fn held(data: Option<&TinySetup>, busy: bool, prompt: bool, put_off: bool) -> Held<'_> {
            Held { data, busy, prompt, put_off }
        }
        // The first read of the server: ask for the outline, and show the screen if it is needed.
        assert_eq!(after_page_read(&owner, &held(None, false, false, false)), Next::Read { prompt: true });
        // Put off, then the server is read again for another reason: the outline is read, not shown.
        assert_eq!(after_page_read(&owner, &held(None, false, false, true)), Next::Read { prompt: false });
        // Held and needed, not shown yet (the server was opened again): shown.
        assert_eq!(after_page_read(&owner, &held(Some(&needed), false, false, false)), Next::Prompt);
        // Already up, or put off: nothing to do.
        assert_eq!(after_page_read(&owner, &held(Some(&needed), false, true, false)), Next::Nothing);
        assert_eq!(after_page_read(&owner, &held(Some(&needed), false, false, true)), Next::Nothing);
        // A read or an answer is out: wait for it.
        assert_eq!(after_page_read(&owner, &held(None, true, false, false)), Next::Nothing);
        assert_eq!(after_page_read(&owner, &held(Some(&needed), true, false, false)), Next::Nothing);
        // Answered and read again: never shown.
        let answered = TinySetup { needed: false, completed_at: "2026-11-01T10:00:00Z".into(), ..needed.clone() };
        assert_eq!(after_page_read(&owner, &held(Some(&answered), false, false, false)), Next::Nothing);
        // Another device answered: the server page now says so, and the outline is read again.
        let mut elsewhere = community(&["first_run_setup"], true);
        elsewhere.server = Some(TinyServerInfo { provider_profile: "none".into(), activity_visibility: "nobody".into(), sign_in_methods: "passkey".into(), setup_completed_at: "2026-11-01T10:00:00Z".into(), ..TinyServerInfo::default() });
        assert_eq!(after_page_read(&elsewhere, &held(Some(&needed), false, true, false)), Next::Read { prompt: true });
        // Anyone else gets nothing, and what was held is let go.
        for other in [community(&["first_run_setup"], false), community(&["roles"], true)] {
            assert_eq!(after_page_read(&other, &held(None, false, false, false)), Next::Nothing);
            assert_eq!(after_page_read(&other, &held(Some(&needed), false, false, false)), Next::Forget);
            assert_eq!(after_page_read(&other, &held(None, false, true, false)), Next::Forget);
        }
    }

    #[test]
    fn what_the_server_keeps_now_is_said_in_the_servers_words() {
        let mut setup = setup();
        let now = right_now(&setup);
        assert_eq!(now.len(), 3);
        assert_eq!(now[0], ("Provider".to_owned(), "full".to_owned(), "full does this.".to_owned()));
        assert_eq!(now[2].1, "any");
        setup.current = posture("none", "nobody", "passkey");
        assert_eq!(right_now(&setup).iter().map(|(_, option, _)| option.as_str()).collect::<Vec<_>>(), ["none", "nobody", "passkey"]);
        // A value the outline does not list (a newer server's) is left out, not guessed.
        setup.current.activity_visibility = "someday".into();
        assert_eq!(right_now(&setup).len(), 2);
        assert!(right_now(&TinySetup::default()).is_empty());
    }

    #[test]
    fn the_outline_is_read_again_when_the_server_says_something_else() {
        let setup = setup();
        let says = |profile: &str, activity: &str, sign_in: &str, done: &str| TinyServerInfo {
            provider_profile: profile.into(),
            activity_visibility: activity.into(),
            sign_in_methods: sign_in.into(),
            setup_completed_at: done.into(),
            ..TinyServerInfo::default()
        };
        assert!(!stale(&setup, None), "no server read, nothing to compare");
        assert!(!stale(&setup, Some(&says("full", "everyone", "any", ""))), "the same");
        // Another device applied privacy.
        assert!(stale(&setup, Some(&says("none", "nobody", "passkey", "2026-11-01T10:00:00Z"))));
        // Another device kept what is there: the choices match, but setup is done.
        assert!(stale(&setup, Some(&says("full", "everyone", "any", "2026-11-01T10:00:00Z"))));
        // One choice changed later, on a server already set up.
        let done = TinySetup { needed: false, completed_at: "2026-11-01T10:00:00Z".into(), ..setup.clone() };
        assert!(!stale(&done, Some(&says("full", "everyone", "any", "2026-11-01T10:00:00Z"))));
        assert!(stale(&done, Some(&says("full", "admins", "any", "2026-11-01T10:00:00Z"))));
        // An older server says none of it.
        assert!(!stale(&setup, Some(&TinyServerInfo::default())));
    }
}
