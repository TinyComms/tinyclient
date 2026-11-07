//! Dev-only seed for the device-link proof: a throwaway tinyserver gets an owner, Ada, and a
//! member, Bea, without any OIDC provider, and one session per DEVICE each person will use.
//! Each session is a separate sign-in, so each device registers its own E2EE identity; nothing
//! here touches keys. Prints JSON on stdout.
//!
//! Adapted from tinyclient scripts/e2e/tiny-seed/main.rs (the lab's seed). Built against
//! tinyserver's `db` crate by server/dl-server.ps1. Never point it at a real community: it signs
//! people in with a made-up issuer.
//!
//! Env: DATABASE_URL_MIGRATE (app_owner), DATABASE_URL (app_runtime), TINY_E2E_BASE_URL,
//!      DL_ADA_DEVICES / DL_BEA_DEVICES (comma-separated session keys).

use std::str::FromStr;

use sha2::Digest;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

const ISSUER: &str = "https://issuer.tiny-e2e.invalid";

fn profile(subject: &str, name: &str) -> db::Profile {
    db::Profile {
        issuer: ISSUER.to_string(),
        subject: subject.to_string(),
        email: Some(format!("{subject}@tiny-e2e.invalid")),
        name: Some(name.to_string()),
        sid: None,
    }
}

fn hash(token: &str) -> Vec<u8> {
    sha2::Sha256::digest(token.as_bytes()).to_vec()
}

fn keys(var: &str, default: &str) -> Vec<String> {
    std::env::var(var)
        .unwrap_or_else(|_| default.to_string())
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let migrate_url = std::env::var("DATABASE_URL_MIGRATE")?;
    let runtime_url = std::env::var("DATABASE_URL")?;
    let base_url = std::env::var("TINY_E2E_BASE_URL")?;
    let owner = PgPoolOptions::new()
        .max_connections(2)
        .connect_with(PgConnectOptions::from_str(&migrate_url)?)
        .await?;
    let runtime = PgPoolOptions::new()
        .max_connections(2)
        .connect_with(PgConnectOptions::from_str(&runtime_url)?)
        .await?;
    db::migrate(&owner).await?;

    // Add one more member to a database that is already seeded (the server stays up): invited by
    // the owner, signed in, N sessions. DL_ADD_ONLY=<subject>, DL_ADD_NAME, DL_ADD_SESSIONS,
    // DL_OWNER_ID, DL_OWNER_ROLE. Prints a JSON array of sessions.
    if let Ok(subject) = std::env::var("DL_ADD_ONLY") {
        let name = std::env::var("DL_ADD_NAME").unwrap_or_else(|_| subject.clone());
        let n: usize = std::env::var("DL_ADD_SESSIONS").ok().and_then(|v| v.parse().ok()).unwrap_or(2);
        let caller = db::Caller {
            user_id: std::env::var("DL_OWNER_ID")?,
            role: std::env::var("DL_OWNER_ROLE").unwrap_or_else(|_| "owner".into()),
            scopes: Vec::new(),
        };
        let db::Wrote::Fresh(issued) = db::create_invite(&runtime, &caller, "member", None)
            .await
            .map_err(|e| format!("invite: {e:?}"))?
        else {
            return Err("invite replayed".into());
        };
        let signed = db::sign_in(&runtime, &profile(&subject, &name), Some(&hash(&issued.token)), db::Groups::default())
            .await
            .map_err(|e| format!("sign-in {subject}: {e:?}"))?;
        let mut out = Vec::new();
        for _ in 0..n {
            let opened = db::open_session(&runtime, &signed.user_id).await.map_err(|e| format!("session: {e:?}"))?;
            out.push(serde_json::json!({
                "base_url": base_url, "token": opened.token, "user_id": signed.user_id,
                "display_name": name, "role": signed.role, "plain_http_ok": true,
            }));
        }
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    let invite = db::owner_invite(&owner, false)
        .await?
        .ok_or("the database already has an owner; seed a fresh database")?;
    let ada = db::sign_in(
        &runtime,
        &profile("ada", "Ada"),
        Some(&hash(&invite.token)),
        db::Groups::default(),
    )
    .await
    .map_err(|e| format!("owner sign-in: {e:?}"))?;
    let caller = db::Caller {
        user_id: ada.user_id.clone(),
        role: ada.role.clone(),
        scopes: Vec::new(),
    };
    // Setup done as the owner would with "Keep what I have" (no change), so Ada's windows open on
    // her conversations, not on the owner's "Set up Tiny server" page.
    let keep = db::SetupChange {
        provider_profile: None,
        activity_visibility: None,
        sign_in_methods: None,
        erase_emails: false,
        allow_lockout: false,
    };
    db::apply_setup(&runtime, &caller, &keep, true)
        .await
        .map_err(|e| format!("setup: {e:?}"))?;
    let db::Wrote::Fresh(issued) = db::create_invite(&runtime, &caller, "member", None)
        .await
        .map_err(|e| format!("invite: {e:?}"))?
    else {
        return Err("invite replayed".into());
    };
    let bea = db::sign_in(
        &runtime,
        &profile("bea", "Bea"),
        Some(&hash(&issued.token)),
        db::Groups::default(),
    )
    .await
    .map_err(|e| format!("sign-in bea: {e:?}"))?;

    let mut sessions = serde_json::Map::new();
    let people = [
        (&ada, "Ada", keys("DL_ADA_DEVICES", "a_desk_a,a_desk_b,a_linux,a_win,a_android,a_ios,a_spare")),
        (&bea, "Bea", keys("DL_BEA_DEVICES", "b_desk")),
    ];
    for (signed, name, devices) in people {
        for key in devices {
            let opened = db::open_session(&runtime, &signed.user_id)
                .await
                .map_err(|e| format!("session {key}: {e:?}"))?;
            sessions.insert(
                key.clone(),
                serde_json::json!({
                    "base_url": base_url,
                    "token": opened.token,
                    "user_id": signed.user_id,
                    "display_name": name,
                    "role": signed.role,
                    "plain_http_ok": true,
                }),
            );
        }
    }
    let out = serde_json::json!({
        "base_url": base_url,
        "people": {
            "ada": { "user_id": ada.user_id, "name": "Ada" },
            "bea": { "user_id": bea.user_id, "name": "Bea" },
        },
        "sessions": sessions,
    });
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}
