//! Signing in to Telegram, one step at a time, as the shell's sheet draws it: a QR code scanned
//! from the phone first; a phone number and the code sent to it otherwise; a login email when
//! Telegram asks for one before the code; a name when the number has no account yet; the second
//! password where the account has one. A recovery email is not used: resetting that password
//! happens in Telegram's own app. `auth.recoverPassword` with no new settings deletes the cloud
//! password, so that recovery stays unimplemented (#542).
//! Passkey sign-in is refused here: a third-party client cannot assert Telegram's passkey domain.
//! Telegram has no web page for this and wants none used.
//!
//! One sign-in at a time. It lives from `SignInBegin` to the step that ends it or to
//! `SignInCancel`, on a wire of its own that is dropped with it; nothing of Telegram's is
//! loaded before it begins. What the user types is used once and forgotten: the code goes into
//! one request, the password into the proof Telegram asks for (it never leaves this process),
//! and neither is stored or printed.

use super::wire::{self, Event, Fault, Wire};
use super::{credentials, open, scripted, supported, unsupported_reason};
use crate::hub::{now, Hub};
use grammers_crypto::two_factor_auth::{calculate_2fa, check_p_and_g};
use grammers_tl_types as tl;
use protocol::{AccountKey, Change, CodeVia, Network, SignInInput, SignInProblem, SignInStep};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

/// Keeps a finished session and starts its accounts.
pub type Adopt = Box<dyn Fn(&str) -> Result<Vec<AccountKey>, String> + Send + Sync>;

/// A QR code is replaced this long before Telegram would stop taking it.
const QR_MARGIN: i64 = 2;

/// Shown instead of starting WebAuthn. `auth.initPasskeyLogin` would ask this client to assert
/// Telegram's relying-party id, which a third-party app cannot do.
const NO_PASSKEY: &str = "Passkey sign-in isn't available in tinyclient. A third-party app cannot use Telegram's passkey domain. Use a QR code or a phone number.";

/// Which way in the user is on.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Way {
    Qr,
    Phone,
    EmailSetup,
    EmailVerify,
    SignUp,
    Passkey,
    /// The second password, and the way that led to it.
    Password { from_qr: bool },
    Ended,
}

struct Sent {
    /// As Telegram is told it: digits after a `+`.
    number: String,
    /// As the user typed it, for the sheet's sentence.
    shown: String,
    hash: String,
}

struct State {
    way: Way,
    step: SignInStep,
    sent: Option<Sent>,
    /// What Telegram says of the account's second password, for the proof.
    password: Option<tl::types::account::Password>,
    /// Masked email address shown on `EmailVerify`.
    email_pattern: String,
    /// Digit length of the email verification code on `EmailVerify`.
    email_code_len: u8,
    /// Terms of service text shown on `SignUp`.
    signup_terms: String,
    /// The phone accepted the code on show; ask Telegram for the outcome.
    scanned: bool,
    /// The user asked for the code again after the network could not be reached.
    retry: bool,
    cancelled: bool,
}

struct Flow {
    wire: Box<dyn Wire>,
    hub: Arc<Hub>,
    adopt: Adopt,
    state: Mutex<State>,
    wake: Condvar,
}

static FLOW: Mutex<Option<Arc<Flow>>> = Mutex::new(None);

fn current() -> MutexGuard<'static, Option<Arc<Flow>>> {
    FLOW.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn waiting() -> SignInStep {
    SignInStep::Qr { link: String::new(), expires: 0, problem: None }
}

/// Computes a Telegram 2FA SRP proof (`InputCheckPasswordSRP`) for `password` by fetching
/// fresh parameters from `account.getPassword`. Used by ownership transfer (`messages.editChatCreator`).
pub(super) fn srp_proof(wire: &dyn Wire, password: &str) -> Result<tl::enums::InputCheckPasswordSrp, Fault> {
    let tl::enums::account::Password::Password(asked) = wire::call(wire, &tl::functions::account::GetPassword {})?;
    let (Some(tl::enums::PasswordKdfAlgo::Sha256Sha256Pbkdf2Hmacsha512iter100000Sha256ModPow(algo)), Some(g_b), Some(srp_id)) =
        (&asked.current_algo, asked.srp_b.clone(), asked.srp_id)
    else {
        return Err(Fault::Rpc { code: 400, name: "PASSWORD_REQUIRED".to_owned(), value: None });
    };
    if !(2..=7).contains(&algo.g) || !check_p_and_g(&algo.p, &algo.g) {
        return Err(Fault::Net("Telegram sent untrusted SRP parameters for the password check.".to_owned()));
    }
    let (salt1, salt2, p, g, secure_random, pw) = (
        algo.salt1.clone(),
        algo.salt2.clone(),
        algo.p.clone(),
        algo.g,
        asked.secure_random.clone(),
        password.as_bytes().to_vec(),
    );
    let srp = std::thread::Builder::new()
        .name("srp-transfer".into())
        .stack_size(4 * 1024 * 1024)
        .spawn(move || calculate_2fa(&salt1, &salt2, &p, &g, g_b, secure_random, &pw))
        .map_err(|err| Fault::Net(err.to_string()))?;
    let (m1, g_a) = srp.join().map_err(|_| Fault::Net("SRP calculation failed.".to_owned()))?;
    Ok(tl::types::InputCheckPasswordSrp { srp_id, a: g_a.to_vec(), m1: m1.to_vec() }.into())
}

/// Begins a sign-in, or with `input` answers the step on show. Answers the step to draw.
/// `Err` when this build cannot sign in to Telegram at all, and why.
pub fn step(hub: &Arc<Hub>, input: Option<SignInInput>, adopt: Adopt) -> Result<SignInStep, String> {
    if !supported() {
        return Err(unsupported_reason());
    }
    let flow = {
        let mut held = current();
        let live = held.as_ref().filter(|flow| flow.state().way != Way::Ended).cloned();
        match (live, &input) {
            (Some(flow), _) => flow,
            // Nothing to answer: the sheet is told to begin again.
            (None, Some(_)) => return Ok(SignInStep::Failed { reason: "That sign-in is over. Start again.".to_owned() }),
            (None, None) => {
                let flow = Arc::new(Flow {
                    wire: open(None)?,
                    hub: hub.clone(),
                    adopt,
                    state: Mutex::new(State {
                        way: Way::Qr,
                        step: waiting(),
                        sent: None,
                        password: None,
                        email_pattern: String::new(),
                        email_code_len: 6,
                        signup_terms: String::new(),
                        scanned: false,
                        retry: false,
                        cancelled: false,
                    }),
                    wake: Condvar::new(),
                });
                flow.start()?;
                *held = Some(flow.clone());
                flow
            }
        }
    };
    Ok(match input {
        None => flow.state().step.clone(),
        Some(input) => flow.answer(input),
    })
}

/// The user closed the sheet. The wire goes with the flow; nothing was kept.
pub fn cancel() {
    if let Some(flow) = current().take() {
        flow.state().cancelled = true;
        flow.wake.notify_all();
    }
}

fn base64url(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn via(kind: &tl::enums::auth::SentCodeType) -> Option<(CodeVia, u8)> {
    use tl::enums::auth::SentCodeType as T;
    let (via, length) = match kind {
        T::App(code) => (CodeVia::App, code.length),
        T::Sms(code) => (CodeVia::Sms, code.length),
        T::Call(code) => (CodeVia::Call, code.length),
        T::FragmentSms(code) => (CodeVia::Sms, code.length),
        T::EmailCode(code) => (CodeVia::Email, code.length),
        // MissedCall, FlashCall, FirebaseSms, SmsWord, SmsPhrase, SetUpEmailRequired are handled separately or unsupported.
        _ => return None,
    };
    Some((via, length.clamp(4, 8) as u8))
}

fn next_via(kind: &Option<tl::enums::auth::CodeType>) -> Option<CodeVia> {
    use tl::enums::auth::CodeType as T;
    Some(match kind.as_ref()? {
        T::Sms | T::FragmentSms => CodeVia::Sms,
        T::Call => CodeVia::Call,
        T::FlashCall | T::MissedCall => return None,
    })
}

/// Masked address and reset times from an email login code. The period Telegram sends is seconds
/// from now; the sheet wants the moment it becomes available.
fn email_meta(kind: &tl::enums::auth::SentCodeType) -> (String, i64, i64) {
    let tl::enums::auth::SentCodeType::EmailCode(code) = kind else {
        return (String::new(), 0, 0);
    };
    let available = code.reset_available_period.map(|seconds| now() + i64::from(seconds)).unwrap_or(0);
    let pending = code.reset_pending_date.map(i64::from).unwrap_or(0);
    (code.email_pattern.clone(), available, pending)
}

/// What a refusal means for the step on show.
fn problem(fault: &Fault) -> SignInProblem {
    if let Some(seconds) = fault.flood() {
        return SignInProblem::TooMany { until: now() + seconds as i64 };
    }
    match fault {
        Fault::Net(_) => SignInProblem::Unreachable,
        Fault::Refused(why) => SignInProblem::Other { reason: why.clone() },
        Fault::Rpc { name, .. } => match name.as_str() {
            "PHONE_NUMBER_INVALID" => SignInProblem::PhoneInvalid,
            "PHONE_NUMBER_UNOCCUPIED" => SignInProblem::PhoneUnknown,
            "PHONE_NUMBER_BANNED" | "PHONE_NUMBER_FLOOD" => SignInProblem::PhoneBanned,
            "PHONE_CODE_INVALID" | "PHONE_CODE_EMPTY" | "CODE_INVALID" | "CODE_EMPTY" => SignInProblem::CodeInvalid,
            "PHONE_CODE_EXPIRED" | "CODE_EXPIRED" | "PASSWORD_RECOVERY_EXPIRED" => SignInProblem::CodeExpired,
            "PASSWORD_HASH_INVALID" => SignInProblem::PasswordInvalid,
            "PHONE_PASSWORD_FLOOD" => SignInProblem::TooMany { until: now() + 3_600 },
            "EMAIL_INVALID" => SignInProblem::Other { reason: "That doesn't look like an email address.".to_owned() },
            "EMAIL_NOT_ALLOWED" => SignInProblem::Other { reason: "Telegram does not allow that email address.".to_owned() },
            "FIRSTNAME_INVALID" => SignInProblem::Other { reason: "Enter a valid first name.".to_owned() },
            "LASTNAME_INVALID" => SignInProblem::Other { reason: "Enter a valid last name.".to_owned() },
            "PASSWORD_RECOVERY_NA" => SignInProblem::Other {
                reason: "No recovery email is set on this account. Reset it in the Telegram app.".to_owned(),
            },
            "PASSKEY_LOGIN_DISABLED" | "AUTH_PASSKEY_UNAVAILABLE" => SignInProblem::Other {
                reason: "Telegram did not allow passkey sign-in for this client. Sign in with a QR code or phone number instead.".to_owned(),
            },
            other => SignInProblem::Other { reason: format!("Telegram refused this ({other}).") },
        },
    }
}

impl Flow {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Two threads for as long as the sign-in lasts: one hears the phone accept the code, the
    /// other keeps a fresh code on show. Both end when the flow is cancelled or finished.
    fn start(self: &Arc<Flow>) -> Result<(), String> {
        let events = self.wire.events().ok_or("the wire has no events")?;
        let heard = Arc::downgrade(self);
        let spawned = std::thread::Builder::new().name("telegram-signin-events".into()).spawn(move || {
            // Ends when the wire is dropped and its events with it.
            for event in events {
                let Some(flow) = heard.upgrade() else {
                    return;
                };
                if matches!(&event, Event::Update { update, .. } if matches!(**update, tl::enums::Update::LoginToken)) {
                    flow.state().scanned = true;
                    flow.wake.notify_all();
                }
            }
        });
        spawned.map_err(|err| err.to_string())?;
        let flow = self.clone();
        let spawned = std::thread::Builder::new().name("telegram-signin".into()).spawn(move || flow.keep_code());
        spawned.map(drop).map_err(|err| err.to_string())
    }

    /// Shows a step, and tells the shell when it changed without being asked.
    fn show(&self, step: SignInStep, announce: bool) -> SignInStep {
        let changed = {
            let mut state = self.state();
            let changed = state.step != step;
            state.step = step.clone();
            changed
        };
        if changed && announce {
            self.hub.changed(Change::SignIn { network: Network::Telegram, step: step.clone() });
        }
        step
    }

    fn api(&self) -> (i32, String) {
        match credentials() {
            Some((id, hash)) => (id, hash.to_owned()),
            // The scripted wire takes any.
            None => (0, String::new()),
        }
    }

    /// While the user is on the QR way: asks for a code, shows it, and asks again when it runs
    /// out or the phone accepts it. Parked, with no timer, on any other way.
    fn keep_code(&self) {
        let (api_id, api_hash) = self.api();
        loop {
            {
                let mut state = self.state();
                loop {
                    if state.cancelled || state.way == Way::Ended {
                        return;
                    }
                    if state.way == Way::Qr {
                        break;
                    }
                    state = self.wake.wait(state).unwrap_or_else(|poisoned| poisoned.into_inner());
                }
                state.scanned = false;
                state.retry = false;
            }
            let request = tl::functions::auth::ExportLoginToken { api_id, api_hash: api_hash.clone(), except_ids: Vec::new() };
            let mut answer = wire::call(self.wire.as_ref(), &request);
            // The account lives in another datacenter: the code is taken there.
            if let Ok(tl::enums::auth::LoginToken::MigrateTo(moved)) = &answer {
                self.wire.set_home(moved.dc_id);
                answer = wire::call(self.wire.as_ref(), &tl::functions::auth::ImportLoginToken { token: moved.token.clone() });
            }
            if self.state().way != Way::Qr {
                continue;
            }
            let until = match answer {
                Ok(tl::enums::auth::LoginToken::Token(token)) => {
                    let link = format!("tg://login?token={}", base64url(&token.token));
                    let expires = i64::from(token.expires);
                    self.show(SignInStep::Qr { link, expires, problem: None }, true);
                    Some(expires - QR_MARGIN)
                }
                Ok(tl::enums::auth::LoginToken::Success(done)) => {
                    self.finish(done.authorization, true);
                    continue;
                }
                Ok(tl::enums::auth::LoginToken::MigrateTo(_)) => Some(now()),
                Err(fault) if fault.is("SESSION_PASSWORD_NEEDED") => {
                    let step = self.password_step(true, None);
                    self.show(step, true);
                    continue;
                }
                // Not reached: the sheet says so and offers to try again. Nothing is retried
                // until the user asks.
                Err(fault) => {
                    let problem = Some(problem(&fault));
                    self.show(SignInStep::Qr { link: String::new(), expires: 0, problem }, true);
                    None
                }
            };
            let mut state = self.state();
            loop {
                if state.cancelled || state.way != Way::Qr || state.scanned || state.retry {
                    break;
                }
                state = match until {
                    Some(until) if until <= now() => break,
                    Some(until) => {
                        let left = Duration::from_secs((until - now()).max(1) as u64);
                        self.wake.wait_timeout(state, left).unwrap_or_else(|poisoned| poisoned.into_inner()).0
                    }
                    None => self.wake.wait(state).unwrap_or_else(|poisoned| poisoned.into_inner()),
                };
            }
        }
    }

    fn answer(&self, input: SignInInput) -> SignInStep {
        let way = self.state().way;
        match (input, way) {
            (SignInInput::UsePhone, Way::Qr) => {
                self.state().way = Way::Phone;
                self.show(SignInStep::Phone { problem: None }, false)
            }
            // Asked again on the QR way: the code is fetched again after a failure.
            (SignInInput::UseQr, Way::Qr) => {
                self.state().retry = true;
                self.wake.notify_all();
                let step = self.state().step.clone();
                match step {
                    SignInStep::Qr { problem: Some(_), .. } => self.show(waiting(), false),
                    step => step,
                }
            }
            (SignInInput::UseQr | SignInInput::Back, Way::Phone) if !matches!(self.state().step, SignInStep::Code { .. }) => {
                {
                    let mut state = self.state();
                    state.way = Way::Qr;
                    state.sent = None;
                }
                let step = self.show(waiting(), false);
                self.wake.notify_all();
                step
            }
            // From the code back to the number.
            (SignInInput::Back, Way::Phone) => {
                self.state().sent = None;
                self.show(SignInStep::Phone { problem: None }, false)
            }
            (SignInInput::Back, Way::Password { from_qr: true }) => {
                {
                    let mut state = self.state();
                    state.way = Way::Qr;
                    state.password = None;
                }
                let step = self.show(waiting(), false);
                self.wake.notify_all();
                step
            }
            (SignInInput::Back, Way::Password { from_qr: false }) => {
                {
                    let mut state = self.state();
                    state.way = Way::Phone;
                    state.password = None;
                    state.sent = None;
                }
                self.show(SignInStep::Phone { problem: None }, false)
            }
            (SignInInput::Phone { number }, Way::Phone) => self.send_code(&number),
            (SignInInput::Resend, Way::Phone) => self.resend(),
            (SignInInput::Code { code }, Way::Phone) => self.check_code(code),
            (SignInInput::Email { email }, Way::EmailSetup) => self.send_login_email(&email),
            (SignInInput::EmailCode { code }, Way::EmailVerify) => self.check_login_email(code),
            (SignInInput::Back, Way::EmailSetup) => {
                {
                    let mut state = self.state();
                    state.way = Way::Phone;
                    state.sent = None;
                }
                self.show(SignInStep::Phone { problem: None }, false)
            }
            (SignInInput::Back, Way::EmailVerify) => {
                let phone = self.state().sent.as_ref().map(|sent| sent.shown.clone()).unwrap_or_default();
                self.state().way = Way::EmailSetup;
                self.show(SignInStep::EmailSetup { phone, problem: None }, false)
            }
            (SignInInput::SignUp { first_name, last_name }, Way::SignUp) => self.create_account(first_name, last_name),
            (SignInInput::Back, Way::SignUp) => {
                {
                    let mut state = self.state();
                    state.way = Way::Phone;
                    state.sent = None;
                }
                self.show(SignInStep::Phone { problem: None }, false)
            }
            (SignInInput::Password { password }, Way::Password { from_qr }) => self.check_password(password, from_qr),
            // `RecoverPassword` and `RecoveryCode` fall through. They are not sent on:
            // `auth.recoverPassword` with no new settings deletes the cloud password (#542).
            // Never `auth.initPasskeyLogin` or `auth.finishPasskeyLogin`: those assert Telegram's domain.
            (SignInInput::UsePasskey | SignInInput::Passkey { .. }, _) => self.refuse_passkey(),
            (SignInInput::Back, Way::Passkey) => {
                self.state().way = Way::Qr;
                let step = self.show(waiting(), false);
                self.wake.notify_all();
                step
            }
            // Not what the step on show takes: it is shown again.
            _ => self.state().step.clone(),
        }
    }

    /// The step a `sentCode` leads to: a login email, the digit boxes, or a stop when the code
    /// arrives in a way this sheet cannot take (a flash call, a word, a phrase).
    fn after_sent_code(&self, sent: &tl::types::auth::SentCode, problem: Option<SignInProblem>) -> SignInStep {
        let shown = self.state().sent.as_ref().map(|held| held.shown.clone()).unwrap_or_default();
        if matches!(sent.r#type, tl::enums::auth::SentCodeType::SetUpEmailRequired(_)) {
            self.state().way = Way::EmailSetup;
            return SignInStep::EmailSetup { phone: shown, problem };
        }
        let Some((via, length)) = via(&sent.r#type) else {
            return self.fail("Telegram wants this code in a way tinyclient cannot take. Sign in with a QR code instead.");
        };
        let (email_pattern, reset_available_at, reset_pending_date) = email_meta(&sent.r#type);
        self.state().way = Way::Phone;
        SignInStep::Code {
            via,
            length,
            phone: shown,
            email_pattern,
            reset_available_at,
            reset_pending_date,
            next: next_via(&sent.next_type),
            resend_at: sent.timeout.map_or(0, |seconds| now() + i64::from(seconds)),
            problem,
        }
    }

    fn send_code(&self, typed: &str) -> SignInStep {
        let digits: String = typed.chars().filter(char::is_ascii_digit).collect();
        if !(7..=15).contains(&digits.len()) {
            return self.show(SignInStep::Phone { problem: Some(SignInProblem::PhoneInvalid) }, false);
        }
        let (api_id, api_hash) = self.api();
        let request = tl::functions::auth::SendCode {
            phone_number: format!("+{digits}"),
            api_id,
            api_hash,
            settings: tl::types::CodeSettings {
                allow_flashcall: false,
                current_number: false,
                allow_app_hash: false,
                allow_missed_call: false,
                allow_firebase: false,
                unknown_number: false,
                logout_tokens: None,
                token: None,
                app_sandbox: None,
            }
            .into(),
        };
        let mut answer = wire::call(self.wire.as_ref(), &request);
        // The number belongs to another datacenter, which Telegram names: asked once more there.
        if let Err(Fault::Rpc { code: 303, value: Some(dc), .. }) = &answer {
            self.wire.set_home(*dc as i32);
            answer = wire::call(self.wire.as_ref(), &request);
        }
        match answer {
            Ok(tl::enums::auth::SentCode::Code(sent)) => {
                self.state().sent = Some(Sent {
                    number: format!("+{digits}"),
                    shown: typed.trim().to_owned(),
                    hash: sent.phone_code_hash.clone(),
                });
                let step = self.after_sent_code(&sent, None);
                self.show(step, false)
            }
            Ok(tl::enums::auth::SentCode::Success(done)) => self.finish(done.authorization, false),
            Ok(tl::enums::auth::SentCode::PaymentRequired(_)) => self.fail("Telegram wants a payment before it sends this number a code. Sign in with a QR code instead."),
            Err(fault) => self.show(SignInStep::Phone { problem: Some(problem(&fault)) }, false),
        }
    }

    /// The code step again with something wrong, or the number step when there is no code out.
    fn code_again(&self, problem: SignInProblem) -> SignInStep {
        let step = match self.state().step.clone() {
            SignInStep::Code { via, length, phone, email_pattern, reset_available_at, reset_pending_date, next, resend_at, .. } => {
                SignInStep::Code { via, length, phone, email_pattern, reset_available_at, reset_pending_date, next, resend_at, problem: Some(problem) }
            }
            _ => SignInStep::Phone { problem: Some(problem) },
        };
        self.show(step, false)
    }

    fn resend(&self) -> SignInStep {
        let Some((number, hash)) = self.state().sent.as_ref().map(|sent| (sent.number.clone(), sent.hash.clone())) else {
            return self.show(SignInStep::Phone { problem: None }, false);
        };
        let request = tl::functions::auth::ResendCode { phone_number: number, phone_code_hash: hash, reason: None };
        match wire::call(self.wire.as_ref(), &request) {
            Ok(tl::enums::auth::SentCode::Code(sent)) => {
                if let Some(held) = &mut self.state().sent {
                    held.hash = sent.phone_code_hash.clone();
                }
                let step = self.after_sent_code(&sent, None);
                self.show(step, false)
            }
            Ok(tl::enums::auth::SentCode::Success(done)) => self.finish(done.authorization, false),
            Ok(tl::enums::auth::SentCode::PaymentRequired(_)) => self.fail("Telegram wants a payment before it sends this number another code."),
            Err(fault) => self.code_again(problem(&fault)),
        }
    }

    fn check_code(&self, code: String) -> SignInStep {
        let Some((number, hash)) = self.state().sent.as_ref().map(|sent| (sent.number.clone(), sent.hash.clone())) else {
            return self.show(SignInStep::Phone { problem: None }, false);
        };
        let request = tl::functions::auth::SignIn {
            phone_number: number,
            phone_code_hash: hash,
            phone_code: Some(code),
            email_verification: None,
        };
        match wire::call(self.wire.as_ref(), &request) {
            Ok(authorization) => self.finish(authorization, false),
            Err(fault) if fault.is("SESSION_PASSWORD_NEEDED") => {
                let step = self.password_step(false, None);
                self.show(step, false)
            }
            Err(fault) => self.code_again(problem(&fault)),
        }
    }

    /// Reads what Telegram says of the second password and shows its step.
    fn password_step(&self, from_qr: bool, problem: Option<SignInProblem>) -> SignInStep {
        match wire::call(self.wire.as_ref(), &tl::functions::account::GetPassword {}) {
            Ok(tl::enums::account::Password::Password(password)) => {
                let hint = password.hint.clone().unwrap_or_default();
                let has_recovery = password.has_recovery;
                let mut state = self.state();
                state.way = Way::Password { from_qr };
                state.password = Some(password);
                SignInStep::Password { hint, has_recovery, problem }
            }
            Err(fault) => {
                self.state().way = Way::Ended;
                SignInStep::Failed { reason: format!("Telegram did not say how to check the password ({fault}).") }
            }
        }
    }

    fn check_password(&self, password: String, from_qr: bool) -> SignInStep {
        let Some(asked) = self.state().password.clone() else {
            return self.password_step(from_qr, None);
        };
        let hint = asked.hint.clone().unwrap_or_default();
        let (Some(tl::enums::PasswordKdfAlgo::Sha256Sha256Pbkdf2Hmacsha512iter100000Sha256ModPow(algo)), Some(g_b), Some(srp_id)) =
            (&asked.current_algo, asked.srp_b.clone(), asked.srp_id)
        else {
            return self.fail("This account's password is kept in a way this version of tinyclient does not know. Update tinyclient.");
        };
        // Telegram's numbers are checked before anything is proved with them: a bad group
        // would give the password away.
        if !(2..=7).contains(&algo.g) || !check_p_and_g(&algo.p, &algo.g) {
            return self.fail("Telegram sent numbers that cannot be trusted for checking the password.");
        }
        // grammers' SRP bends a debug build's 256 KiB connection stack past its end, so the
        // computation runs on its own thread with room to spare.
        let srp = std::thread::Builder::new().name("srp".into()).stack_size(4 * 1024 * 1024).spawn({
            let (salt1, salt2, p, g, secure_random, password) = (
                algo.salt1.clone(),
                algo.salt2.clone(),
                algo.p.clone(),
                algo.g,
                asked.secure_random.clone(),
                password.clone(),
            );
            move || calculate_2fa(&salt1, &salt2, &p, &g, g_b, secure_random, password.as_bytes())
        })
        .expect("the SRP thread");
        let (m1, g_a) = srp.join().expect("the SRP thread");
        drop(password);
        let request = tl::functions::auth::CheckPassword {
            password: tl::types::InputCheckPasswordSrp { srp_id, a: g_a.to_vec(), m1: m1.to_vec() }.into(),
        };
        match wire::call(self.wire.as_ref(), &request) {
            Ok(authorization) => self.finish(authorization, false),
            Err(fault) => {
                // Each try needs fresh numbers from Telegram, right or wrong.
                let step = self.password_step(from_qr, Some(problem(&fault)));
                let step = match step {
                    SignInStep::Password { has_recovery, problem, .. } => SignInStep::Password { hint, has_recovery, problem },
                    other => other,
                };
                self.show(step, false)
            }
        }
    }

    fn sent_pair(&self) -> Option<(String, String)> {
        self.state().sent.as_ref().map(|sent| (sent.number.clone(), sent.hash.clone()))
    }

    fn login_purpose(&self, number: String, hash: String) -> tl::enums::EmailVerifyPurpose {
        tl::types::EmailVerifyPurposeLoginSetup { phone_number: number, phone_code_hash: hash }.into()
    }

    fn send_login_email(&self, typed: &str) -> SignInStep {
        let email = typed.trim();
        let phone = self.state().sent.as_ref().map(|sent| sent.shown.clone()).unwrap_or_default();
        if email.is_empty() {
            return self.show(SignInStep::EmailSetup { phone, problem: Some(SignInProblem::Other { reason: "Enter an email address.".to_owned() }) }, false);
        }
        let Some((number, hash)) = self.sent_pair() else {
            return self.show(SignInStep::Phone { problem: None }, false);
        };
        let request = tl::functions::account::SendVerifyEmailCode { purpose: self.login_purpose(number, hash), email: email.to_owned() };
        match wire::call(self.wire.as_ref(), &request) {
            Ok(tl::enums::account::SentEmailCode::Code(sent)) => {
                let length = sent.length.clamp(4, 8) as u8;
                let mut state = self.state();
                state.way = Way::EmailVerify;
                state.email_pattern = sent.email_pattern.clone();
                state.email_code_len = length;
                let step = SignInStep::EmailVerify { email_pattern: sent.email_pattern, length, problem: None };
                drop(state);
                self.show(step, false)
            }
            Err(fault) => self.show(SignInStep::EmailSetup { phone, problem: Some(problem(&fault)) }, false),
        }
    }

    fn email_verify_again(&self, problem: SignInProblem) -> SignInStep {
        let (email_pattern, length) = {
            let state = self.state();
            (state.email_pattern.clone(), state.email_code_len)
        };
        self.show(SignInStep::EmailVerify { email_pattern, length, problem: Some(problem) }, false)
    }

    fn check_login_email(&self, code: String) -> SignInStep {
        let Some((number, hash)) = self.sent_pair() else {
            return self.show(SignInStep::Phone { problem: None }, false);
        };
        let request = tl::functions::account::VerifyEmail {
            purpose: self.login_purpose(number.clone(), hash.clone()),
            verification: tl::types::EmailVerificationCode { code: code.clone() }.into(),
        };
        match wire::call(self.wire.as_ref(), &request) {
            Ok(tl::enums::account::EmailVerified::Login(login)) => match login.sent_code {
                tl::enums::auth::SentCode::Code(sent) => {
                    if let Some(held) = &mut self.state().sent {
                        held.hash = sent.phone_code_hash.clone();
                    }
                    let step = self.after_sent_code(&sent, None);
                    self.show(step, false)
                }
                tl::enums::auth::SentCode::Success(done) => self.finish(done.authorization, false),
                tl::enums::auth::SentCode::PaymentRequired(_) => self.fail("Telegram wants a payment before it sends this number a code. Sign in with a QR code instead."),
            },
            Ok(tl::enums::account::EmailVerified::Verified(_)) => {
                let request = tl::functions::auth::SignIn {
                    phone_number: number,
                    phone_code_hash: hash,
                    phone_code: None,
                    email_verification: Some(tl::types::EmailVerificationCode { code }.into()),
                };
                match wire::call(self.wire.as_ref(), &request) {
                    Ok(authorization) => self.finish(authorization, false),
                    Err(fault) if fault.is("SESSION_PASSWORD_NEEDED") => {
                        let step = self.password_step(false, None);
                        self.show(step, false)
                    }
                    Err(fault) => self.email_verify_again(problem(&fault)),
                }
            }
            Err(fault) => self.email_verify_again(problem(&fault)),
        }
    }

    fn begin_signup(&self, required: tl::types::auth::AuthorizationSignUpRequired, announce: bool) -> SignInStep {
        let terms = match required.terms_of_service {
            Some(tl::enums::help::TermsOfService::Service(terms)) => terms.text,
            None => String::new(),
        };
        let phone = self.state().sent.as_ref().map(|sent| sent.shown.clone()).unwrap_or_default();
        {
            let mut state = self.state();
            state.way = Way::SignUp;
            state.signup_terms = terms.clone();
        }
        self.show(SignInStep::SignUp { phone, terms, problem: None }, announce)
    }

    fn signup_again(&self, problem: SignInProblem) -> SignInStep {
        let (phone, terms) = {
            let state = self.state();
            let phone = state.sent.as_ref().map(|sent| sent.shown.clone()).unwrap_or_default();
            (phone, state.signup_terms.clone())
        };
        self.show(SignInStep::SignUp { phone, terms, problem: Some(problem) }, false)
    }

    fn create_account(&self, first_name: String, last_name: String) -> SignInStep {
        let first_name = first_name.trim().to_owned();
        let last_name = last_name.trim().to_owned();
        if first_name.is_empty() {
            return self.signup_again(SignInProblem::Other { reason: "Enter a valid first name.".to_owned() });
        }
        let Some((number, hash)) = self.sent_pair() else {
            return self.show(SignInStep::Phone { problem: None }, false);
        };
        let request = tl::functions::auth::SignUp { no_joined_notifications: false, phone_number: number, phone_code_hash: hash, first_name, last_name };
        match wire::call(self.wire.as_ref(), &request) {
            Ok(authorization) => self.finish(authorization, false),
            Err(fault) => self.signup_again(problem(&fault)),
        }
    }

    fn refuse_passkey(&self) -> SignInStep {
        self.state().way = Way::Passkey;
        self.show(
            SignInStep::Passkey {
                options: String::new(),
                problem: Some(SignInProblem::Other { reason: NO_PASSKEY.to_owned() }),
            },
            false,
        )
    }

    fn fail(&self, reason: &str) -> SignInStep {
        self.state().way = Way::Ended;
        self.show(SignInStep::Failed { reason: reason.to_owned() }, false)
    }

    /// Telegram let us in: the session is kept and the account started.
    fn finish(&self, authorization: tl::enums::auth::Authorization, announce: bool) -> SignInStep {
        let user = match authorization {
            tl::enums::auth::Authorization::SignUpRequired(required) => return self.begin_signup(required, announce),
            tl::enums::auth::Authorization::Authorization(tl::types::auth::Authorization { user: tl::enums::User::User(user), .. }) => user,
            tl::enums::auth::Authorization::Authorization(_) => {
                return self.show(SignInStep::Phone { problem: Some(SignInProblem::PhoneUnknown) }, announce);
            }
        };
        self.wire.signed_in(&user);
        let first = user.first_name.clone().unwrap_or_default();
        let name = match user.last_name.as_deref() {
            Some(last) if !last.is_empty() => format!("{first} {last}").trim().to_owned(),
            _ => first,
        };
        let fresh = self.wire.session(user.id, &name);
        // A second sign-in joins the accounts already stored. Replacing the item would
        // sign the first one out.
        let session = match crate::session::read(Network::Telegram) {
            crate::session::Stored::Absent => fresh,
            crate::session::Stored::Found(existing) => match super::combine(&existing, &fresh) {
                Ok(joined) => joined,
                Err(_) => {
                    let _ = wire::call(self.wire.as_ref(), &tl::functions::auth::LogOut {});
                    return self.fail("The other Telegram account could not be read, so this sign-in was not kept.");
                }
            },
            crate::session::Stored::Unreadable(_) => {
                let _ = wire::call(self.wire.as_ref(), &tl::functions::auth::LogOut {});
                return self.fail("The other Telegram account could not be read, so this sign-in was not kept.");
            }
        };
        let step = match (self.adopt)(&session) {
            // The step names who just signed in. The list also holds the accounts already there.
            Ok(accounts) => {
                let id = user.id.to_string();
                SignInStep::Done { account: accounts.into_iter().find(|key| key.account == id) }
            }
            Err(why) => {
                tracing::warn!("chatd: Telegram: the sign-in was not kept: {}", super::super::net::redact(&why));
                // The session exists on Telegram but not here: it is ended there, so the
                // account's device list does not keep one nobody can use.
                let _ = wire::call(self.wire.as_ref(), &tl::functions::auth::LogOut {});
                let keychain = why.starts_with("keychain");
                SignInStep::Failed {
                    reason: if keychain { format!("tinyclient couldn't save the sign-in to {}.", crate::session::STORE) } else { why },
                }
            }
        };
        self.state().way = Way::Ended;
        self.wake.notify_all();
        let step = self.show(step, announce);
        // The flow is over; its wire goes when the last thread lets go of it.
        let mut held = current();
        if held.as_ref().is_some_and(|flow| std::ptr::eq(flow.as_ref(), self)) {
            held.take();
        }
        step
    }
}

/// Whether a sign-in is on its way, for a test.
#[allow(dead_code)]
pub(super) fn busy() -> bool {
    current().is_some() && scripted()
}
