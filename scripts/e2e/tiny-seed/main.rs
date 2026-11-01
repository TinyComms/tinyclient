//! Dev-only seed for a local tinyserver: makes an owner and two members without any OIDC
//! provider, opens a session for each, and creates one voice room. Prints JSON on stdout.
//!
//! Built against tinyserver's `db` crate by scripts/e2e/local-tiny-call.ps1. Never point it at a
//! real community: it signs people in with a made-up issuer.
//!
//! Env: DATABASE_URL_MIGRATE (app_owner), DATABASE_URL (app_runtime), TINY_E2E_BASE_URL.

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

    let invite = db::owner_invite(&owner, false)
        .await?
        .ok_or("the database already has an owner; seed a fresh database")?;
    let a = db::sign_in(
        &runtime,
        &profile("alice", "Alice (desktop)"),
        Some(&hash(&invite.token)),
        db::Groups::default(),
    )
    .await
    .map_err(|e| format!("owner sign-in: {e:?}"))?;
    let caller = db::Caller {
        user_id: a.user_id.clone(),
        role: a.role.clone(),
        scopes: Vec::new(),
    };

    let mut people = vec![(a.user_id.clone(), a.role.clone(), "Alice (desktop)", "a")];
    for (subject, name, key) in [("bea", "Bea (android)", "b"), ("cy", "Cy (ios)", "c")] {
        let db::Wrote::Fresh(issued) = db::create_invite(&runtime, &caller, "member", None)
            .await
            .map_err(|e| format!("invite: {e:?}"))?
        else {
            return Err("invite replayed".into());
        };
        let signed = db::sign_in(
            &runtime,
            &profile(subject, name),
            Some(&hash(&issued.token)),
            db::Groups::default(),
        )
        .await
        .map_err(|e| format!("sign-in {subject}: {e:?}"))?;
        people.push((signed.user_id, signed.role, name, key));
    }

    let db::Wrote::Fresh(room) = db::create_room(&runtime, &caller, &db::NewRoom::named("Proof"), None)
        .await
        .map_err(|e| format!("room: {e:?}"))?
    else {
        return Err("room replayed".into());
    };

    let mut sessions = serde_json::Map::new();
    for (user_id, role, name, key) in people {
        let opened = db::open_session(&runtime, &user_id)
            .await
            .map_err(|e| format!("session: {e:?}"))?;
        sessions.insert(
            key.to_string(),
            serde_json::json!({
                "base_url": base_url,
                "token": opened.token,
                "user_id": user_id,
                "display_name": name,
                "role": role,
                "plain_http_ok": true,
            }),
        );
    }
    let out = serde_json::json!({
        "base_url": base_url,
        "room": { "id": room.id, "name": room.name },
        "sessions": sessions,
    });
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}
