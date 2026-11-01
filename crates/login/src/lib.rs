//! The web sign-in's logic, with no window: which page each network signs in on, what is read
//! from it (Discord's token probe, Slack's `localConfig_v2` and `d` cookie, Teams' PKCE
//! authorize, redirect and code exchange), the session line chatd keeps, and the exit codes.
//!
//! Two users: the `tinyclient-login` helper binary (a desktop window, `main.rs`, built with the
//! default `window` feature), and chatd on a phone, which drives the platform's own web view with
//! [`web::sign_in`] through `protocol::web_sign_in`. chatd links this library without `window`,
//! so no wry or tao is built for a phone.

pub mod common;
pub mod teams;
pub mod web;

pub use common::*;
