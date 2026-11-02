//! `--shot` only: a made-up step for design review, so every face of the sheet can be drawn
//! with no Telegram and no sign-in begun. `TINYCLIENT_SIGNIN_SHOT` names the state: `qr-waiting`,
//! `qr`, `qr-offline`, `phone`, `phone-error`, `code`, `code-sms`, `code-wrong`, `code-expired`,
//! `code-toomany`, `code-later`, `password`, `password-wrong`, `failed`, `unsupported`; a Tiny
//! server's `tiny` (the code to paste), `tiny-error`, `tiny-invite` (a code pasted) and
//! `tiny-browser` (waiting on the browser); Google Messages' `messages-browser`,
//! `messages-waiting` and `messages-emoji`; and, as a phone draws them, Discord's, Slack's and
//! Teams's own-page sign-in: `discord`, `discord-waiting`, `slack`, `teams`.
//! chatd is never asked about the sign-in while one is on show.

use super::{now, Face};
use crate::gui::time::clock;
use protocol::{CodeVia, Network, SignInProblem, SignInStep};

/// What to draw, and what the fields hold as if it had been typed.
pub(super) struct Canned {
    /// Whose sheet: Telegram's, unless the state is another network's.
    pub network: Network,
    pub face: Face,
    pub number: &'static str,
    pub digits: &'static str,
    pub password: &'static str,
    pub local: Option<SignInProblem>,
    /// The invite code, as if pasted.
    pub invite: String,
}

/// The demo's Tiny server, as its invites name it.
const TINY: &str = "https://nightshift.example";

/// A link of the shape Telegram issues: the scheme, then 43 letters of base64url. It signs
/// nobody in.
const LINK: &str = "tg://login?token=AQJ3kd0P7m2xYbV5cRt8hLwZq1NnEoU6sGfTiKy4DaM";

const PHONE: &str = "+1 555 010 0142";

/// The next 3:42 PM, so the words read as they do in the design.
fn later() -> i64 {
    let minute = now() / 60 + 1;
    (minute..minute + 24 * 60).map(|minute| minute * 60).find(|at| clock(*at) == "3:42 PM").unwrap_or(minute * 60)
}

fn code(via: CodeVia, next: Option<CodeVia>, resend_at: i64, problem: Option<SignInProblem>) -> SignInStep {
    SignInStep::Code { via, length: 5, phone: PHONE.to_owned(), next, resend_at, problem }
}

pub(super) fn canned() -> Option<Canned> {
    let name = std::env::var("TINYCLIENT_SIGNIN_SHOT").ok().filter(|name| !name.is_empty())?;
    let qr = |link: &str, problem| SignInStep::Qr { link: link.to_owned(), expires: 0, problem };
    let password = |problem| SignInStep::Password { hint: "the old street".to_owned(), problem };
    let sms = Some(CodeVia::Sms);
    let mut canned = Canned { network: Network::Telegram, face: Face::Step(qr("", None)), number: "", digits: "", password: "", local: None, invite: String::new() };
    let web = |network, started| Some(Canned { network, face: Face::Web { started }, number: "", digits: "", password: "", local: None, invite: String::new() });
    let step = match name.as_str() {
        "discord" => return web(Network::Discord, false),
        "discord-waiting" => return web(Network::Discord, true),
        "slack" => return web(Network::Slack, false),
        "teams" => return web(Network::Teams, false),
        "tiny" => {
            canned.network = Network::Tiny;
            SignInStep::Invite { server: String::new(), problem: None }
        }
        "tiny-error" => {
            canned.network = Network::Tiny;
            SignInStep::Invite { server: String::new(), problem: Some(SignInProblem::Other { reason: "That code isn't one this server gave out. Ask for a new one.".to_owned() }) }
        }
        "messages-browser" => SignInStep::Browser { server: "https://messages.google.com".into(), provider: "Google".into(), waiting: false, problem: None },
        "messages-waiting" => SignInStep::Browser { server: "https://messages.google.com".into(), provider: "Google".into(), waiting: true, problem: None },
        "messages-emoji" => SignInStep::Emoji { emoji: "🐢".into() },
        "qr-waiting" => qr("", None),
        "qr" => qr(LINK, None),
        "qr-offline" => qr("", Some(SignInProblem::Unreachable)),
        "phone" => {
            canned.number = "555 010 0142";
            SignInStep::Phone { problem: None }
        }
        "phone-error" => {
            canned.number = "555 01";
            canned.local = Some(SignInProblem::PhoneInvalid);
            SignInStep::Phone { problem: None }
        }
        "phone-unknown" => {
            canned.number = "555 010 0142";
            SignInStep::Phone { problem: Some(SignInProblem::PhoneUnknown) }
        }
        "code" => {
            canned.digits = "419";
            code(CodeVia::App, sms, 0, None)
        }
        "code-sms" => code(CodeVia::Sms, Some(CodeVia::Call), 0, None),
        "code-later" => code(CodeVia::App, sms, later(), None),
        "code-wrong" => code(CodeVia::App, sms, 0, Some(SignInProblem::CodeInvalid)),
        "code-expired" => code(CodeVia::App, sms, 0, Some(SignInProblem::CodeExpired)),
        "code-toomany" => code(CodeVia::App, sms, 0, Some(SignInProblem::TooMany { until: later() })),
        "password" => {
            canned.password = "hunter2!";
            password(None)
        }
        "password-wrong" => password(Some(SignInProblem::PasswordInvalid)),
        "tiny-invite" => {
            canned.network = Network::Tiny;
            canned.invite = protocol::pack_join(TINY, "Nf3kQ8xLw2pZ7rT1").unwrap_or_default();
            SignInStep::Invite { server: String::new(), problem: None }
        }
        "tiny-browser" => {
            canned.network = Network::Tiny;
            SignInStep::Browser { server: TINY.to_owned(), provider: "Pocket ID".to_owned(), waiting: true, problem: None }
        }
        "failed" => SignInStep::Failed { reason: "Telegram ended this sign-in. Start again to get a new code.".to_owned() },
        "unsupported" => {
            canned.face = Face::Unsupported("This build of tinyclient has no Telegram app id, so it can't sign in to Telegram.".to_owned());
            return Some(canned);
        }
        _ => return None,
    };
    canned.face = Face::Step(step);
    Some(canned)
}
