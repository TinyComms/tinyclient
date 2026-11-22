use super::{bindings, emit, wayland_keys, Action, Binding, Notice, PAUSED};
use async_io::Timer;
use futures_lite::{
    future::{block_on, or},
    StreamExt,
};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use zbus::message::{Message, Type};
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};
use zbus::{Connection, MatchRule, MessageStream, Proxy};

const DESKTOP: &str = "org.freedesktop.portal.Desktop";
const PATH: &str = "/org/freedesktop/portal/desktop";
const SHORTCUTS: &str = "org.freedesktop.portal.GlobalShortcuts";
const REQUEST: &str = "org.freedesktop.portal.Request";
const SESSION: &str = "org.freedesktop.portal.Session";
const TICK: Duration = Duration::from_millis(100);
const RESPONSE_LIMIT: Duration = Duration::from_secs(120);

type Labels = [Option<String>; 4];
type Properties = HashMap<String, OwnedValue>;
type Shortcuts = Vec<(String, Properties)>;

static TRIGGERS: Mutex<Labels> = Mutex::new([None, None, None, None]);

pub(super) fn has_shortcuts() -> bool {
    TRIGGERS
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .iter()
        .any(Option::is_some)
}

pub(super) fn trigger(action: Action) -> Option<String> {
    TRIGGERS.lock().unwrap_or_else(|err| err.into_inner())[action as usize].clone()
}

fn publish(next: Labels, toggles: &Sender<Notice>) {
    let mut current = TRIGGERS.lock().unwrap_or_else(|err| err.into_inner());
    if *current != next {
        *current = next;
        let _ = toggles.send(Notice::PortalChanged);
    }
}

pub(super) fn watch(toggles: &Sender<Notice>) {
    block_on(async {
        let mut attempted = [None; 4];
        loop {
            let bound = bindings();
            if bound != attempted {
                attempted = bound;
                if bound
                    .iter()
                    .flatten()
                    .any(|binding| wayland_keys::preferred_trigger(*binding).is_some())
                {
                    let result = run(bound, toggles).await;
                    if let Err(error) = result {
                        tracing::debug!(%error, "tinyclient: Wayland shortcut session unavailable");
                    }
                }
            }
            Timer::after(TICK).await;
        }
    });
}

/// Tokens scope responses to this connection and let us subscribe before a fast portal can reply.
fn token() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    format!("tiny_voice_{}", NEXT.fetch_add(1, Ordering::Relaxed))
}

fn sender_path(conn: &Connection) -> Result<String, String> {
    Ok(conn
        .unique_name()
        .ok_or("No portal connection name.")?
        .as_str()
        .trim_start_matches(':')
        .replace('.', "_"))
}

async fn listen(conn: &Connection, rule: MatchRule<'_>) -> Result<MessageStream, zbus::Error> {
    MessageStream::for_match_rule(rule, conn, Some(64)).await
}

async fn request<B>(
    conn: &Connection,
    proxy: &Proxy<'_>,
    method: &str,
    token: &str,
    body: &B,
) -> Result<Properties, String>
where
    B: serde::ser::Serialize + zbus::zvariant::DynamicType,
{
    let path = format!("{PATH}/request/{}/{token}", sender_path(conn)?);
    let rule = MatchRule::builder()
        .msg_type(Type::Signal)
        .sender(DESKTOP)
        .map_err(words)?
        .interface(REQUEST)
        .map_err(words)?
        .member("Response")
        .map_err(words)?
        .path(path.as_str())
        .map_err(words)?
        .build();
    let mut responses = listen(conn, rule).await.map_err(words)?;
    let response = async {
        let handle: OwnedObjectPath = proxy.call(method, body).await.map_err(words)?;
        if handle.as_str() != path {
            return Err(
                "The global shortcuts portal returned an unexpected request path.".to_owned(),
            );
        }
        let reply = responses
            .next()
            .await
            .ok_or("The global shortcuts portal disconnected.")?
            .map_err(words)?;
        let (code, result): (u32, Properties) = reply.body().deserialize().map_err(words)?;
        if code == 0 {
            Ok(result)
        } else {
            Err("The desktop did not grant global shortcuts.".to_owned())
        }
    };
    or(response, async {
        Timer::after(RESPONSE_LIMIT).await;
        Err("The desktop did not answer the global shortcuts request.".to_owned())
    })
    .await
}

fn words(error: impl std::fmt::Display) -> String {
    error.to_string()
}

async fn run(bound: [Option<Binding>; 4], toggles: &Sender<Notice>) -> Result<(), String> {
    let conn = Connection::session().await.map_err(words)?;
    let proxy = Proxy::new(&conn, DESKTOP, PATH, SHORTCUTS)
        .await
        .map_err(words)?;
    let request_token = token();
    let session_token = token();
    let options = HashMap::from([
        ("handle_token", Value::from(request_token.as_str())),
        ("session_handle_token", Value::from(session_token.as_str())),
    ]);
    let mut created = request(&conn, &proxy, "CreateSession", &request_token, &options).await?;
    // Older portals encode this object path as a string in the response vardict.
    let returned = created
        .remove("session_handle")
        .ok_or("No global shortcuts session handle.")?;
    let session = if let Ok(path) = <&str>::try_from(&returned) {
        OwnedObjectPath::try_from(path).map_err(words)?
    } else {
        OwnedObjectPath::try_from(returned).map_err(words)?
    };
    let session_proxy = Proxy::new(&conn, DESKTOP, session.as_str(), SESSION)
        .await
        .map_err(words)?;
    let result = bind(&conn, &proxy, &session, bound, toggles).await;
    publish(Labels::default(), toggles);
    or(
        async {
            let _ = session_proxy.call::<_, _, ()>("Close", &()).await;
        },
        async {
            Timer::after(Duration::from_secs(2)).await;
        },
    )
    .await;
    result
}

async fn bind(
    conn: &Connection,
    proxy: &Proxy<'_>,
    session: &OwnedObjectPath,
    bound: [Option<Binding>; 4],
    toggles: &Sender<Notice>,
) -> Result<(), String> {
    // One stream preserves the portal's ordering even if press and release are both queued.
    let rule = MatchRule::builder()
        .msg_type(Type::Signal)
        .sender(DESKTOP)
        .map_err(words)?
        .path_namespace(PATH)
        .map_err(words)?
        .build();
    let mut events = listen(conn, rule).await.map_err(words)?;
    let mut owner = proxy.receive_owner_changed().await.map_err(words)?;
    let shortcuts = Action::ALL
        .iter()
        .zip(bound)
        .filter_map(|(action, binding)| {
            let trigger = wayland_keys::preferred_trigger(binding?)?;
            let props = HashMap::from([
                ("description", Value::from(action.name())),
                ("preferred_trigger", Value::from(trigger)),
            ]);
            Some((action.key(), props))
        })
        .collect::<Vec<_>>();
    let request_token = token();
    let options = HashMap::from([("handle_token", Value::from(request_token.as_str()))]);
    let mut result = request(
        conn,
        proxy,
        "BindShortcuts",
        &request_token,
        &(session, shortcuts, "", options),
    )
    .await?;
    let registered = result
        .remove("shortcuts")
        .map(Shortcuts::try_from)
        .transpose()
        .map_err(words)?
        .unwrap_or_default();
    if bindings() != bound {
        return Ok(());
    }

    let mut active = labels(registered, &bound);
    publish(active.clone(), toggles);
    let mut keys = wayland_keys::Keys::default();
    let mut deadline = Instant::now() + TICK;
    let result = loop {
        let event = or(events.next(), async {
            owner.next().await;
            None
        });
        match or(async { Some(event.await) }, async {
            Timer::at(deadline).await;
            None
        })
        .await
        {
            Some(Some(Ok(event))) => {
                let header = event.header();
                let interface = header.interface().map(|v| v.as_str());
                let member = header.member().map(|v| v.as_str());
                match (interface, member) {
                    (Some(SESSION), Some("Closed"))
                        if header
                            .path()
                            .is_some_and(|path| path.as_str() == session.as_str()) =>
                    {
                        break Ok(())
                    }
                    (Some(SHORTCUTS), Some("ShortcutsChanged")) => {
                        if let Ok((path, changed)) =
                            event.body().deserialize::<(OwnedObjectPath, Shortcuts)>()
                        {
                            if &path == session {
                                let next = labels(changed, &bound);
                                if active != next {
                                    release(&mut keys, toggles);
                                    keys = wayland_keys::Keys::default();
                                    active = next;
                                    publish(active.clone(), toggles);
                                }
                            }
                        }
                    }
                    (Some(SHORTCUTS), Some(member @ ("Activated" | "Deactivated"))) => {
                        if let Some(action) = action_for(&event, session, &active) {
                            if let Some(down) = keys.edge(
                                action,
                                member == "Activated",
                                PAUSED.load(Ordering::Relaxed),
                            ) {
                                if !emit(action, down, toggles) {
                                    break Ok(());
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            Some(_) => break Err("The global shortcuts connection was lost.".to_owned()),
            None => {
                if bindings() != bound {
                    break Ok(());
                }
                if PAUSED.load(Ordering::Relaxed) {
                    release(&mut keys, toggles);
                }
                deadline = Instant::now() + TICK;
            }
        }
    };
    release(&mut keys, toggles);
    result
}

fn labels(shortcuts: Shortcuts, bound: &[Option<Binding>; 4]) -> Labels {
    let mut labels = Labels::default();
    for (id, properties) in shortcuts {
        if let Some(action) = Action::ALL.into_iter().find(|action| action.key() == id) {
            let at = action as usize;
            labels[at] = bound[at]
                .and_then(wayland_keys::preferred_trigger)
                .and_then(|_| {
                    let trigger = <&str>::try_from(properties.get("trigger_description")?).ok()?;
                    (!trigger.is_empty()).then(|| trigger.to_owned())
                });
        }
    }
    labels
}

fn action_for(event: &Message, session: &OwnedObjectPath, active: &Labels) -> Option<Action> {
    let (path, id, _, _): (OwnedObjectPath, String, u64, Properties) =
        event.body().deserialize().ok()?;
    if &path != session {
        return None;
    }
    Action::ALL
        .into_iter()
        .find(|action| action.key() == id && active[*action as usize].is_some())
}

fn release(keys: &mut wayland_keys::Keys, toggles: &Sender<Notice>) {
    for (action, held) in [Action::Talk, Action::Priority]
        .into_iter()
        .zip(keys.release_talk())
    {
        if held {
            let _ = emit(action, false, toggles);
        }
    }
}
