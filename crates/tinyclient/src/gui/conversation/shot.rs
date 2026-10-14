//! For `--shot` only: puts the pane in a state worth a screenshot before the frame is written,
//! and prints what was measured. Read from the environment so no flag is needed in `mod.rs`:
//!
//! - `TINYCLIENT_SHOT_CONVERSATION=<id>`: show another conversation of the open one's account,
//!   or `network/account/id` one of any account.
//! - `TINYCLIENT_SHOT_AT=<message id>`: jump to it, as a reply quote would.
//! - `TINYCLIENT_SHOT_UP=<points>`: scroll up this far from the bottom.
//! - `TINYCLIENT_SHOT_THREAD=<message id>`: open the thread under it.
//! - `TINYCLIENT_SHOT_VIEW=<message id>`: open the viewer on its first picture.
//! - `TINYCLIENT_SHOT_ZOOM=<percent>`: with `VIEW`, zoom the viewer to it (`100`, `250`).
//! - `TINYCLIENT_SHOT_VIEW_STEPS=<n>`: with `VIEW`, go on through that many pictures, which
//!   decodes more than the ceiling holds, and print what was dropped.
//! - `TINYCLIENT_SHOT_HOVER=<message id>`: show the toolbar on it, as the pointer would.
//! - `TINYCLIENT_SHOT_MENU=<message id>`: open its context menu.
//! - `TINYCLIENT_SHOT_PICKER=<message id>`: open the reaction picker for it.
//! - `TINYCLIENT_SHOT_CONFIRM=<message id>`: open the delete dialog for it.
//! - `TINYCLIENT_SHOT_SELECT=<message id>`: select all of its text.
//! - `TINYCLIENT_SHOT_REVEAL=1`: open every spoilered picture.
//! - `TINYCLIENT_SHOT_STATS=1`: print the time to open, the memory footprint and decoded bytes.
//! - `TINYCLIENT_SHOT_PAGE_BACK=<messages>`: page that far into history and come back.
//! - `TINYCLIENT_SHOT_STEPS=<step;step;…>`: things the user does to the window, 120 ms apart
//!   from the first conversation drawn (use with `--open`), before the picture: `open:N` (row N of the list on show), `members`,
//!   `details` (the header's buttons), `thread` (the first thread in the open conversation),
//!   `back`, `forward`, `close` (the pane's ✕), `escape` (in the composer, with nothing of its
//!   own up), `members-on` (the channel switch, as a wider window left it), `wait:ms` (let `--mode` and `--open` land first). Prints the pane after each.

use super::media;
use super::{Pane, PaneEvent};
use gpui::{px, Context, ListOffset, Window};
use std::time::Duration;

#[derive(Default)]
pub struct Script {
    at: Option<String>,
    up: Option<f32>,
    thread: Option<String>,
    view: Option<String>,
    zoom: Option<f32>,
    view_steps: usize,
    hover: Option<String>,
    menu: Option<String>,
    picker: Option<String>,
    confirm: Option<String>,
    select: Option<String>,
    reveal: bool,
    /// The states that need a row's place have been set up.
    staged: bool,
    stats: bool,
    page_back: Option<usize>,
    opened: bool,
    drawn: bool,
    /// Messages asked for so far while paging back.
    paged: usize,
    settled: bool,
    /// A message an older page was to leave in place, and where it was.
    held: Option<(String, f32)>,
}

impl Script {
    pub fn paging(&self) -> bool {
        self.page_back.is_some()
    }

    pub fn held(&mut self, id: &str, below: f32) {
        if self.stats {
            self.held = Some((id.to_owned(), below));
        }
    }

    /// The toolbar is staged on a message: the real pointer, which is nowhere, does not take it.
    pub fn holds_hover(&self) -> bool {
        self.hover.is_some() || self.menu.is_some() || self.picker.is_some()
    }

    pub fn from_env() -> Script {
        let var = |name: &str| std::env::var(name).ok().filter(|value| !value.is_empty());
        Script {
            at: var("TINYCLIENT_SHOT_AT"),
            up: var("TINYCLIENT_SHOT_UP").and_then(|points| points.parse().ok()),
            thread: var("TINYCLIENT_SHOT_THREAD"),
            view: var("TINYCLIENT_SHOT_VIEW"),
            zoom: var("TINYCLIENT_SHOT_ZOOM").and_then(|percent| percent.parse::<f32>().ok()).map(|percent| percent / 100.),
            view_steps: var("TINYCLIENT_SHOT_VIEW_STEPS").and_then(|steps| steps.parse().ok()).unwrap_or(0),
            hover: var("TINYCLIENT_SHOT_HOVER"),
            menu: var("TINYCLIENT_SHOT_MENU"),
            picker: var("TINYCLIENT_SHOT_PICKER"),
            confirm: var("TINYCLIENT_SHOT_CONFIRM"),
            select: var("TINYCLIENT_SHOT_SELECT"),
            reveal: var("TINYCLIENT_SHOT_REVEAL").is_some(),
            staged: false,
            stats: var("TINYCLIENT_SHOT_STATS").is_some(),
            page_back: var("TINYCLIENT_SHOT_PAGE_BACK").and_then(|messages| messages.parse().ok()),
            opened: false,
            drawn: false,
            paged: 0,
            settled: false,
            held: None,
        }
    }
}

/// `TINYCLIENT_SHOT_CONVERSATION=<id>`: show this conversation of the open one's account instead,
/// for one the Inbox does not list.
/// `network/account/id` names one of any account (`telegram/7000000/g8001`).
pub fn instead(open: &protocol::Conversation) -> Option<protocol::Conversation> {
    let id = std::env::var("TINYCLIENT_SHOT_CONVERSATION").ok().filter(|id| !id.is_empty())?;
    let mut other = open.clone();
    let named = id.splitn(3, '/').collect::<Vec<_>>();
    let network = named.first().and_then(|word| protocol::Network::ALL.into_iter().find(|network| network.label().eq_ignore_ascii_case(word)));
    match (network, named.as_slice()) {
        (Some(network), [_, account, id]) => other.key = protocol::Key { network, account: (*account).to_owned(), id: (*id).to_owned() },
        _ => other.key.id = id,
    }
    Some(other)
}

/// What Activity Monitor calls Memory, now and at its highest since launch, in MB.
fn footprint() -> (f64, f64) {
    #[cfg(target_os = "macos")]
    {
        // SAFETY: the buffer is a zeroed `rusage_info_v4`, which is what the V4 flavor fills in.
        let (now, peak) = unsafe {
            let mut info: libc::rusage_info_v4 = std::mem::zeroed();
            libc::proc_pid_rusage(libc::getpid(), libc::RUSAGE_INFO_V4, (&raw mut info).cast());
            (info.ri_phys_footprint, info.ri_lifetime_max_phys_footprint)
        };
        return (now as f64 / 1_048_576., peak as f64 / 1_048_576.);
    }
    #[cfg(not(target_os = "macos"))]
    (0., 0.)
}

fn report(pane: &Pane, what: &str) {
    let (now, peak) = footprint();
    eprintln!(
        "s5: {what}: footprint {now:.1} MB (highest {peak:.1} MB), {} messages held, pictures {:.1} MB (peak {:.1} MB, {} dropped)",
        pane.window.messages.len(),
        pane.images.bytes() as f64 / 1_048_576.,
        pane.images.peak as f64 / 1_048_576.,
        pane.images.evicted,
    );
}

/// The first page of the conversation has been applied.
pub fn opened(pane: &mut Pane, cx: &mut Context<Pane>) {
    if pane.is_thread() || std::mem::replace(&mut pane.script.opened, true) {
        return;
    }
    if pane.script.stats {
        let took = pane.open.as_ref().map(|open| open.started.elapsed());
        eprintln!("s5: first page applied {took:?} after opening");
    }
    if let Some(id) = pane.script.at.clone() {
        pane.jump_to(&id, cx);
    }
    if let Some(root) = pane.script.thread.clone().and_then(|id| pane.find(&id)) {
        cx.emit(PaneEvent::OpenThread(root));
    }
    let Some(messages) = pane.script.page_back else {
        return;
    };
    cx.spawn(async move |this, cx| {
        cx.background_executor().timer(Duration::from_millis(120)).await;
        let _ = this.update(cx, |this, _| report(this, "open"));
        let mut paged = 0;
        while paged < messages {
            // The screenshot is taken a second after opening, so this does not wait for the
            // list to reach the top by itself: it asks for the page before, as reaching it does.
            let more = this.update(cx, |this, cx| {
                let first = this.window.messages.first().map(|first| first.id.clone());
                match first.filter(|_| this.window.older && !this.loading_older) {
                    Some(id) => {
                        this.script.paged += protocol::PAGE_LIMIT as usize;
                        this.load(protocol::Page::Before { id }, super::Purpose::Older, cx);
                    }
                    None => return this.loading_older,
                }
                true
            });
            if !matches!(more, Ok(true)) {
                break;
            }
            cx.background_executor().timer(Duration::from_millis(4)).await;
            paged = this.read_with(cx, |this, _| this.script.paged).unwrap_or(messages);
        }
        // Draw where it ended up, at the top of what is held, before measuring.
        let _ = this.update(cx, |this, cx| {
            this.list.scroll_to(ListOffset::default());
            cx.notify();
        });
        cx.background_executor().timer(Duration::from_millis(120)).await;
        let _ = this.update(cx, |this, cx| {
            report(this, &format!("paged back {paged}"));
            this.go_to_bottom(cx);
        });
        cx.background_executor().timer(Duration::from_millis(150)).await;
        let _ = this.update(cx, |this, _| report(this, "back at the newest"));
    })
    .detach();
}

/// The rows have been laid out.
pub fn drawn(pane: &mut Pane, window: &mut Window, cx: &mut Context<Pane>) {
    if pane.is_thread() || !pane.script.opened {
        return;
    }
    // A step at a time: the list only knows the heights of rows it has drawn.
    if let Some(left) = pane.script.up.filter(|left| *left > 0.) {
        let step = left.min(400.);
        pane.scroll_by(px(-step));
        pane.script.up = Some(left - step);
        cx.notify();
    }
    if let Some((id, was)) = pane.script.held.take() {
        // The frame after an older page landed: where the message that was on screen is now.
        let top = pane.list.viewport_bounds().top();
        let now = pane.seen.borrow().iter().find_map(|(ix, bounds)| {
            let message = pane.items.get(*ix).and_then(super::rows::Item::message)?;
            (message.id == id).then(|| f32::from(bounds.top() - top))
        });
        eprintln!("s5: older page landed: {id} was {was:.1} below the top, is now {now:.1?}");
    }
    stage(pane, window, cx);
    if pane.script.stats && pane.script.drawn && pane.script.up.is_none_or(|left| left <= 0.) {
        // Once more when everything asked for has been decoded and drawn.
        let settled = pane.images.settled();
        if settled && !std::mem::replace(&mut pane.script.settled, true) {
            report(pane, "settled");
        }
    }
    if std::mem::replace(&mut pane.script.drawn, true) {
        return;
    }
    if pane.script.stats {
        let took = pane.open.as_ref().map(|open| open.started.elapsed());
        eprintln!("s5: first rows drawn {took:?} after opening");
        report(pane, "first draw");
    }
    let viewed = pane.script.view.clone().and_then(|id| pane.find(&id));
    if let Some(message) = viewed {
        let first = message.attachments.iter().position(|attachment| media::shape(attachment).is_visual());
        if let Some(first) = first {
            pane.open_viewer(&message.id, first, window, cx);
            if let Some(zoom) = pane.script.zoom {
                pane.zoom(Some(zoom), cx);
            }
            let steps = pane.script.view_steps;
            if steps > 0 {
                cx.spawn(async move |this, cx| {
                    for _ in 0..steps {
                        cx.background_executor().timer(Duration::from_millis(70)).await;
                        let _ = this.update(cx, |this, cx| this.step(1, cx));
                    }
                    cx.background_executor().timer(Duration::from_millis(200)).await;
                    let _ = this.update(cx, |this, _| report(this, &format!("viewed {steps} more")));
                })
                .detach();
            }
        }
    }
}

/// Sets up what needs to know where a row is, once that row has been drawn.
fn stage(pane: &mut Pane, window: &mut Window, cx: &mut Context<Pane>) {
    if pane.script.staged {
        return;
    }
    if pane.script.reveal {
        let hidden = pane.window.messages.iter().flat_map(|message| &message.attachments).filter(|a| media::is_spoiler(a));
        let ids: Vec<String> = hidden.map(|attachment| attachment.file.id.clone()).collect();
        pane.shown.extend(ids);
    }
    if let Some(message) = pane.script.select.clone().and_then(|id| pane.find(&id)) {
        let blocks = super::rich::blocks(&message.text, &message.spans);
        pane.selection =
            Some(super::Selection { message: message.id.clone(), anchor: (0, 0), head: Some((blocks.len(), usize::MAX)) });
    }
    let wanted = [&pane.script.hover, &pane.script.menu, &pane.script.picker, &pane.script.confirm];
    let Some(id) = wanted.into_iter().flatten().next().cloned() else {
        pane.script.staged = true;
        cx.notify();
        return;
    };
    // Where the row is, to put the menu where a click on it would have been.
    let place = pane.seen.borrow().iter().find_map(|(ix, bounds)| {
        let message = pane.items.get(*ix).and_then(super::rows::Item::message)?;
        (message.id == id).then_some(*bounds)
    });
    let (Some(bounds), Some(message)) = (place, pane.find(&id)) else {
        return;
    };
    pane.script.staged = true;
    pane.hover = Some(id.clone());
    let at = gpui::point(bounds.right() - px(120.), bounds.top() + px(12.));
    if pane.script.menu.is_some() {
        pane.open_menu(message, at, cx);
    } else if pane.script.picker.is_some() {
        pane.open_picker(&id, gpui::point(bounds.right() - px(60.), bounds.top()), window, cx);
    } else if pane.script.confirm.is_some() {
        pane.delete(&message, false, window, cx);
    }
    cx.notify();
}

/// Runs `TINYCLIENT_SHOT_STEPS` once.
pub fn steps(cx: &mut gpui::Context<crate::gui::state::Shell>) {
    let Some(steps) = std::env::var("TINYCLIENT_SHOT_STEPS").ok().filter(|steps| !steps.is_empty()) else {
        return;
    };
    cx.spawn(async move |this, cx| {
        cx.background_executor().timer(Duration::from_millis(100)).await;
        for step in steps.split(';').map(str::trim).filter(|step| !step.is_empty()) {
            // `wait:1500` lets the shot's own staging (`--mode`, `--open`) land first.
            if let Some(ms) = step.strip_prefix("wait:").and_then(|ms| ms.parse().ok()) {
                cx.background_executor().timer(Duration::from_millis(ms)).await;
                for handle in cx.update(|cx| cx.windows()) {
                    let _ = gpui::AppContext::update_window(cx, handle, |_, window, cx| drop(window.draw(cx)));
                }
                continue;
            }
            let step = step.to_owned();
            let _ = this.update(cx, |shell, cx| {
                match step.split_once(':') {
                    Some(("open", row)) => {
                        if let Ok(row) = row.parse() {
                            shell.open_listed(row, cx);
                        }
                    }
                    _ => match step.as_str() {
                        "members" => shell.toggle_member_list(cx),
                        "details" => shell.toggle_details(cx),
                        "back" => shell.travel(true, cx),
                        "forward" => shell.travel(false, cx),
                        "close" => shell.close_right(cx),
                        // Escape in the composer with nothing of its own to close.
                        "escape" => shell.composer_escaped(cx),
                        "members-on" => shell.members_switch_on(cx),
                        "thread" => {
                            let pane = shell.conversation_state.pane.get().cloned();
                            let found = pane.and_then(|pane| {
                                let pane = pane.read(cx);
                                let root = pane.window.messages.iter().find(|message| message.thread.is_some()).cloned()?;
                                Some((pane.key()?.clone(), root))
                            });
                            if let Some((conversation, root)) = found {
                                shell.open_thread_of(conversation, root, cx);
                            }
                        }
                        other => eprintln!("shot: unknown step {other}"),
                    },
                }
                cx.notify();
            });
            cx.background_executor().timer(Duration::from_millis(40)).await;
            // A window behind others gets no frames from macOS: draw what changed, so the
            // picture shows the last step.
            for handle in cx.update(|cx| cx.windows()) {
                let _ = gpui::AppContext::update_window(cx, handle, |_, window, cx| drop(window.draw(cx)));
            }
            let _ = this.update(cx, |shell, cx| {
                let open = shell.snapshot.as_ref().and_then(|s| s.open.as_ref()).map(|open| open.title.clone());
                let stack = &shell.conversation_state.side.stack;
                let thread = shell.conversation_state.thread.borrow().as_ref().and_then(|thread| thread.read(cx).key().cloned());
                eprintln!(
                    "shot: after {step}: open {open:?}, pane {:?} over {:?}, slot {:?}, thread view {:?}",
                    stack.push.as_ref().map(crate::gui::pane::Layer::title),
                    stack.base.as_ref().map(crate::gui::pane::Layer::title),
                    shell.right_kind(),
                    thread.map(|key| key.id),
                );
            });
        }
    })
    .detach();
}
