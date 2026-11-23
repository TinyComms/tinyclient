//! A sign-in tinyclient draws itself, one step at a time, for a network that has no web page to
//! show (Telegram). The shell asks chatd to begin, draws the step it is told, and answers it;
//! chatd tells the step again whenever it moves on its own (`Change::SignIn`). A Tiny server signs
//! in through the system browser: the sheet only says it is waiting (`SignInStep::Browser`).
//!
//! What the user types (a number, a code, a password) crosses the local socket once, inside
//! [`SignInInput`], and is never stored or logged by either side.

use crate::AccountKey;
use serde::{Deserialize, Serialize};
use std::fmt;

/// How a login code reached the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodeVia {
    /// A message in the network's own app on another device.
    App,
    Sms,
    Call,
    Email,
    Other,
}

/// Why the step is being shown again. The shell has the words for each.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "problem", rename_all = "snake_case")]
pub enum SignInProblem {
    /// The network could not be reached.
    Unreachable,
    PhoneInvalid,
    /// No account uses this number, and Telegram did not offer to create one.
    PhoneUnknown,
    PhoneBanned,
    CodeInvalid,
    CodeExpired,
    PasswordInvalid,
    /// Not an http or https host. A user in the address is refused. A path, query, or fragment is dropped.
    UrlInvalid,
    /// Signed in on the provider, but not a member of the Tiny server, and no invite came with it.
    NotMember,
    /// The network will not take another try before `until` (unix seconds).
    TooMany { until: i64 },
    /// Anything else, in words the user can read.
    Other { reason: String },
}

fn is_zero(value: &i64) -> bool {
    *value == 0
}

fn is_u32_zero(value: &u32) -> bool {
    *value == 0
}

fn is_u64_zero(value: &u64) -> bool {
    *value == 0
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// Where an optional post-link message history transfer stands (`SignInStep::Transfer`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferStage {
    /// Waiting for the primary phone to confirm and upload the history archive.
    Waiting,
    /// Downloading and verifying the encrypted history archive.
    Downloading,
    /// Streaming and importing chats and messages into the local store.
    Importing,
    /// The primary phone declined or skipped sending message history.
    Declined,
    /// The transfer window or archive expired before download finished.
    Expired,
    /// The transfer failed; the device is still linked and can continue without history.
    Failed,
}

/// Where a sign-in stands: what the shell draws now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "step", rename_all = "snake_case")]
pub enum SignInStep {
    /// Scan `link` as a QR code with the network's phone app. Empty until the network has issued
    /// one. The network replaces it at `expires` (unix seconds); the new one is a `Change`.
    Qr {
        #[serde(default, skip_serializing_if = "String::is_empty")]
        link: String,
        #[serde(default)]
        expires: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        problem: Option<SignInProblem>,
    },
    /// Ask for the phone number, in international form.
    Phone {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        problem: Option<SignInProblem>,
    },
    /// A code of `length` digits was sent `via`, for the number `phone` as the user typed it.
    Code {
        via: CodeVia,
        length: u8,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        phone: String,
        /// Masked email address when `via` is `CodeVia::Email` (`m***@example.com`).
        #[serde(default, skip_serializing_if = "String::is_empty")]
        email_pattern: String,
        /// Unix seconds when an email login reset becomes available, if Telegram gave one.
        #[serde(default, skip_serializing_if = "is_zero")]
        reset_available_at: i64,
        /// Unix seconds when a pending email login reset completes, if one is in flight.
        #[serde(default, skip_serializing_if = "is_zero")]
        reset_pending_date: i64,
        /// How `Resend` would send it, when the network offers another way.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        next: Option<CodeVia>,
        /// Unix seconds before which `Resend` is refused. 0: any time.
        #[serde(default)]
        resend_at: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        problem: Option<SignInProblem>,
    },
    /// Telegram requires a login email before sending a sign-in code.
    EmailSetup {
        #[serde(default, skip_serializing_if = "String::is_empty")]
        phone: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        problem: Option<SignInProblem>,
    },
    /// Verify the login email with the code Telegram just sent to `email_pattern`.
    EmailVerify {
        #[serde(default, skip_serializing_if = "String::is_empty")]
        email_pattern: String,
        #[serde(default)]
        length: u8,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        problem: Option<SignInProblem>,
    },
    /// The account has a second password. `hint` is the user's own, as the network keeps it.
    Password {
        #[serde(default, skip_serializing_if = "String::is_empty")]
        hint: String,
        /// True when Telegram reports a recovery email. This client does not start recovery (#542):
        /// `auth.recoverPassword` with no new settings deletes the cloud password.
        #[serde(default, skip_serializing_if = "is_false")]
        has_recovery: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        problem: Option<SignInProblem>,
    },
    /// Kept so an older step still parses. This client does not collect a recovery code (#542):
    /// `auth.recoverPassword` with no new settings deletes the cloud password.
    PasswordRecovery {
        #[serde(default, skip_serializing_if = "String::is_empty")]
        email_pattern: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        problem: Option<SignInProblem>,
    },
    /// Sign in with a passkey. `options` is the WebAuthn request options JSON from Telegram.
    Passkey {
        #[serde(default, skip_serializing_if = "String::is_empty")]
        options: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        problem: Option<SignInProblem>,
    },
    /// Telegram says this phone number has no account yet and may be registered with a name.
    SignUp {
        #[serde(default, skip_serializing_if = "String::is_empty")]
        phone: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        terms: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        problem: Option<SignInProblem>,
    },
    /// Ask for a Tiny server's address. `problem` is skipped when there is none.
    Server {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        problem: Option<SignInProblem>,
    },
    /// Ask for an invite code for `server`.
    Invite {
        server: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        problem: Option<SignInProblem>,
    },
    /// The rest happens in the system browser, at `provider` (its name, as `server` gives it).
    /// `waiting`: chatd is listening for the browser to come back. Not waiting, with a problem:
    /// it stopped, and `OpenBrowser` begins again.
    Browser {
        server: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        provider: String,
        #[serde(default)]
        waiting: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        problem: Option<SignInProblem>,
    },
    /// Google Messages asks the user to select this emoji on the paired phone.
    Emoji { emoji: String },
    /// Linked to Signal; waiting for or importing optional message history from the primary phone.
    Transfer {
        stage: TransferStage,
        #[serde(default, skip_serializing_if = "is_u32_zero")]
        chats: u32,
        #[serde(default, skip_serializing_if = "is_u32_zero")]
        messages: u32,
        #[serde(default, skip_serializing_if = "is_u64_zero")]
        downloaded_bytes: u64,
        #[serde(default, skip_serializing_if = "is_u64_zero")]
        total_bytes: u64,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        detail: String,
    },
    /// Signed in. The account is in `Status` from now on.
    Done {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        account: Option<AccountKey>,
    },
    /// It cannot go on. `reason` is in words; beginning again starts over.
    Failed { reason: String },
}

/// What the user did about a step.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "input", rename_all = "snake_case")]
pub enum SignInInput {
    /// Scan a code instead of typing a number.
    UseQr,
    /// Type a number instead of scanning.
    UsePhone,
    /// Start passkey sign-in (`auth.initPasskeyLogin`).
    UsePasskey,
    Phone { number: String },
    Code { code: String },
    /// Submit an email address for login email setup (`account.sendVerifyEmailCode`).
    Email { email: String },
    /// Submit the verification code emailed to the login email (`account.verifyEmail`).
    EmailCode { code: String },
    Password { password: String },
    /// Does not recover the password and does not remove it (#542). See `SignInStep::PasswordRecovery`.
    RecoverPassword,
    /// Does not submit a recovery code and does not remove the password. See `RecoverPassword`.
    RecoveryCode { code: String },
    /// Finish passkey sign-in with a WebAuthn assertion (`auth.finishPasskeyLogin`).
    Passkey {
        id: String,
        raw_id: String,
        client_data: String,
        authenticator_data: Vec<u8>,
        signature: Vec<u8>,
        user_handle: String,
    },
    /// Create a new Telegram account after `authorizationSignUpRequired` (`auth.signUp`).
    SignUp {
        first_name: String,
        #[serde(default)]
        last_name: String,
    },
    /// A Tiny server's address, as typed.
    Server { url: String },
    /// A Tiny server's address, to join it with a code.
    HaveCode { url: String },
    /// The invite code, as pasted.
    Invite { code: String },
    /// Show the browser's page again while chatd waits; after it stopped, begin it again.
    OpenBrowser,
    /// Send the code again, the way `SignInStep::Code::next` says.
    Resend,
    /// Skip an optional post-link step (such as Signal message history transfer) and finish.
    Skip,
    /// One step back: from the code to the number, from the number to the QR code.
    Back,
}

/// Never prints what was typed: a request may be logged whole.
impl fmt::Debug for SignInInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            SignInInput::UseQr => "UseQr",
            SignInInput::UsePhone => "UsePhone",
            SignInInput::UsePasskey => "UsePasskey",
            SignInInput::Phone { .. } => "Phone { .. }",
            SignInInput::Code { .. } => "Code { .. }",
            SignInInput::Email { .. } => "Email { .. }",
            SignInInput::EmailCode { .. } => "EmailCode { .. }",
            SignInInput::Password { .. } => "Password { .. }",
            SignInInput::RecoverPassword => "RecoverPassword",
            SignInInput::RecoveryCode { .. } => "RecoveryCode { .. }",
            SignInInput::Passkey { .. } => "Passkey { .. }",
            SignInInput::SignUp { .. } => "SignUp { .. }",
            SignInInput::Server { .. } => "Server { .. }",
            SignInInput::HaveCode { .. } => "HaveCode { .. }",
            SignInInput::Invite { .. } => "Invite { .. }",
            SignInInput::OpenBrowser => "OpenBrowser",
            SignInInput::Resend => "Resend",
            SignInInput::Skip => "Skip",
            SignInInput::Back => "Back",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_emoji_challenge_round_trips() {
        let json = r#"{"step":"emoji","emoji":"🐢"}"#;
        let step: SignInStep = serde_json::from_str(json).expect("Google account pairing challenge");
        assert_eq!(serde_json::to_string(&step).unwrap(), json);
    }

    #[test]
    fn what_was_typed_is_never_printed_and_bare_steps_read() {
        let typed = SignInInput::Password { password: "hunter2".into() };
        assert!(!format!("{typed:?}").contains("hunter2"));
        assert!(!format!("{:?}", SignInInput::Code { code: "12345".into() }).contains("12345"));
        assert!(!format!("{:?}", SignInInput::Email { email: "secret@example.com".into() }).contains("secret"));
        assert!(!format!("{:?}", SignInInput::EmailCode { code: "654321".into() }).contains("654321"));
        assert!(!format!("{:?}", SignInInput::RecoveryCode { code: "999888".into() }).contains("999888"));
        assert!(!format!("{:?}", SignInInput::SignUp { first_name: "Mira".into(), last_name: "Kessler".into() }).contains("Mira"));
        assert!(!format!("{:?}", SignInInput::Passkey {
            id: "cred-1".into(),
            raw_id: "raw-1".into(),
            client_data: "{\"challenge\":\"secret\"}".into(),
            authenticator_data: vec![1, 2],
            signature: vec![3, 4],
            user_handle: "u1".into(),
        }).contains("secret"));
        let url = SignInInput::Server { url: "http://127.0.0.1:8080".into() };
        assert_eq!(format!("{url:?}"), "Server { .. }");
        assert!(!format!("{url:?}").contains("127.0.0.1"));
        let invite = SignInInput::Invite { code: "secret-code".into() };
        assert_eq!(format!("{invite:?}"), "Invite { .. }");
        assert!(!format!("{invite:?}").contains("secret-code"));
        assert_eq!(serde_json::to_string(&SignInInput::OpenBrowser).unwrap(), r#"{"input":"open_browser"}"#);
        // A step with nothing but its name still reads: every other field is optional.
        assert_eq!(serde_json::from_str::<SignInStep>(r#"{"step":"server"}"#).unwrap(), SignInStep::Server { problem: None });
        let browser = SignInStep::Browser { server: "http://127.0.0.1:8080".into(), provider: "Tinyauth".into(), waiting: true, problem: None };
        assert_eq!(serde_json::to_string(&browser).unwrap(), r#"{"step":"browser","server":"http://127.0.0.1:8080","provider":"Tinyauth","waiting":true}"#);
        let not_member = SignInStep::Browser { server: "https://chat.example".into(), provider: String::new(), waiting: false, problem: Some(SignInProblem::NotMember) };
        let json = serde_json::to_string(&not_member).unwrap();
        assert_eq!(json, r#"{"step":"browser","server":"https://chat.example","waiting":false,"problem":{"problem":"not_member"}}"#);
        assert_eq!(serde_json::from_str::<SignInStep>(&json).unwrap(), not_member);
        assert_eq!(serde_json::from_str::<SignInStep>(r#"{"step":"phone"}"#).unwrap(), SignInStep::Phone { problem: None });
        assert_eq!(
            serde_json::from_str::<SignInStep>(r#"{"step":"qr"}"#).unwrap(),
            SignInStep::Qr { link: String::new(), expires: 0, problem: None }
        );
        let code = SignInStep::Code {
            via: CodeVia::App,
            length: 5,
            phone: String::new(),
            email_pattern: String::new(),
            reset_available_at: 0,
            reset_pending_date: 0,
            next: None,
            resend_at: 0,
            problem: None,
        };
        assert_eq!(serde_json::to_string(&code).unwrap(), r#"{"step":"code","via":"app","length":5,"resend_at":0}"#);
        assert_eq!(
            serde_json::from_str::<SignInStep>(r#"{"step":"password","hint":"old"}"#).unwrap(),
            SignInStep::Password { hint: "old".into(), has_recovery: false, problem: None }
        );
        assert_eq!(
            serde_json::from_str::<SignInStep>(r#"{"step":"email_setup"}"#).unwrap(),
            SignInStep::EmailSetup { phone: String::new(), problem: None }
        );
        assert_eq!(
            serde_json::from_str::<SignInStep>(r#"{"step":"password_recovery","email_pattern":"m***@example.com"}"#).unwrap(),
            SignInStep::PasswordRecovery { email_pattern: "m***@example.com".into(), problem: None }
        );
        assert_eq!(
            serde_json::from_str::<SignInStep>(r#"{"step":"passkey"}"#).unwrap(),
            SignInStep::Passkey { options: String::new(), problem: None }
        );
        assert_eq!(
            serde_json::from_str::<SignInStep>(r#"{"step":"sign_up"}"#).unwrap(),
            SignInStep::SignUp { phone: String::new(), terms: String::new(), problem: None }
        );
        let transfer = SignInStep::Transfer {
            stage: TransferStage::Importing,
            chats: 3,
            messages: 42,
            downloaded_bytes: 1024,
            total_bytes: 2048,
            detail: String::new(),
        };
        let encoded = serde_json::to_string(&transfer).unwrap();
        assert_eq!(serde_json::from_str::<SignInStep>(&encoded).unwrap(), transfer);
        assert_eq!(
            serde_json::from_str::<SignInStep>(r#"{"step":"transfer","stage":"waiting"}"#).unwrap(),
            SignInStep::Transfer {
                stage: TransferStage::Waiting,
                chats: 0,
                messages: 0,
                downloaded_bytes: 0,
                total_bytes: 0,
                detail: String::new(),
            }
        );
        assert_eq!(serde_json::to_string(&SignInInput::Skip).unwrap(), r#"{"input":"skip"}"#);
    }
}
