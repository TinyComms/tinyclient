//! Telegram Stories (#540): per-person unseen-story ring metadata and the Story Viewer overlay
//! (photo/video display, progress/timer bar, caption, reactions, next/prev navigation, own-story
//! view count, and private reply field).
//!
//! Privacy rule: `Request::ReadStory` is sent ONLY when the user explicitly opens a story or
//! navigates to it in the viewer — never during ring loading or prefetching.

use super::components::{button, glyph, icon_button, scrim, ButtonKind};
use super::input::TextInput;
use super::state::Shell;
use super::theme::{self, Type};
use gpui::{div, img, prelude::*, px, AnyElement, Context, Div, Entity, MouseButton, Task, Window};
use protocol::{AccountKey, Key, PeerStoryRing, Request, Response, Story, StoryMediaKind};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Quick reaction choices offered in the story viewer footer.
const QUICK_REACTIONS: &[&str] = &["❤️", "🔥", "👍", "😂", "😮", "👏"];
/// Default auto-advance duration for a photo story.
const PHOTO_STORY_MS: u32 = 6_000;

/// Story ring and viewer state owned by [`Shell`].
#[derive(Default)]
pub(super) struct State {
    /// Active story rings keyed by peer (`Key`).
    pub rings: HashMap<Key, PeerStoryRing>,
    /// Currently open story viewer, when the user clicked a ringed avatar or forwarded story card.
    pub viewer: Option<Viewer>,
}

/// Active story viewer session.
pub(super) struct Viewer {
    pub account: AccountKey,
    pub peer: Key,
    pub author_name: String,
    pub stories: Vec<Story>,
    pub index: usize,
    pub loading: bool,
    pub error: Option<String>,
    pub reply_input: Entity<TextInput>,
    pub reply_sending: bool,
    pub reply_status: Option<String>,
    pub started_at: Instant,
    /// Story IDs for which we already sent `Request::ReadStory` in this viewer session.
    pub marked_read: HashSet<String>,
    pub timer: Option<Task<()>>,
}

impl Viewer {
    pub fn current(&self) -> Option<&Story> {
        self.stories.get(self.index)
    }

    pub fn current_mut(&mut self) -> Option<&mut Story> {
        self.stories.get_mut(self.index)
    }

    /// Duration of the currently displayed story in milliseconds (used by the progress bar).
    pub fn current_duration_ms(&self) -> u32 {
        self.current()
            .and_then(|s| s.duration_ms)
            .filter(|&ms| ms >= 1_500)
            .unwrap_or(PHOTO_STORY_MS)
    }

    /// Progress fraction (`0.0..=1.0`) for the current segment bar.
    pub fn progress(&self) -> f32 {
        let total = self.current_duration_ms().max(1) as f32;
        (self.started_at.elapsed().as_millis() as f32 / total).clamp(0.0, 1.0)
    }

    /// Advances to the next story. Returns `false` if already at the last story.
    pub fn next(&mut self) -> bool {
        if self.index + 1 < self.stories.len() {
            self.index += 1;
            self.started_at = Instant::now();
            self.reply_status = None;
            true
        } else {
            false
        }
    }

    /// Moves to the previous story. Returns `false` if already at the first story.
    pub fn prev(&mut self) -> bool {
        if self.index > 0 {
            self.index -= 1;
            self.started_at = Instant::now();
            self.reply_status = None;
            true
        } else {
            false
        }
    }
}

/// Returns the unseen story ring for `peer`, if any unseen stories exist.
pub(super) fn unseen_ring<'a>(rings: &'a HashMap<Key, PeerStoryRing>, peer: &Key) -> Option<&'a PeerStoryRing> {
    rings.get(peer).filter(|r| r.has_unseen())
}

/// Wraps an avatar element in the Telegram unseen-story ring (accent ring, or green for
/// Close Friends).
pub(super) fn wrap_avatar_ring(avatar: Div, size: f32, unseen: bool, close_friends: bool) -> Div {
    let color = if !unseen {
        theme::border_strong()
    } else if close_friends {
        theme::positive()
    } else {
        theme::accent()
    };
    let pad = if size >= 32. { 2. } else { 1.5 };
    div()
        .flex_none()
        .p(px(pad))
        .rounded_full()
        .border_2()
        .border_color(color)
        .child(avatar)
}

/// Selects the initial story index when opening a peer's stories: an explicitly requested story id
/// wins, otherwise the first unseen story, otherwise the first story.
pub(super) fn initial_story_index(stories: &[Story], target_story_id: Option<&str>) -> usize {
    if let Some(target) = target_story_id {
        if let Some(pos) = stories.iter().position(|s| s.id == target) {
            return pos;
        }
    }
    stories
        .iter()
        .position(|s| !s.seen)
        .unwrap_or(0)
}

/// Formats story expiry/posted relative text for the viewer header.
pub(super) fn story_time_label(story: &Story) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let age = (now - story.posted_at).max(0);
    if age < 60 {
        "Just now".to_owned()
    } else if age < 3_600 {
        format!("{}m ago", age / 60)
    } else {
        format!("{}h ago", age / 3_600)
    }
}

impl Shell {
    /// Applies a `Change::Stories` update from chatd to both `story_state` and `people::Hub`.
    pub(super) fn stories_changed(&mut self, account: &AccountKey, rings: &[PeerStoryRing], cx: &mut Context<Self>) {
        self.story_state.rings.retain(|k, _| k.account() != *account);
        for ring in rings {
            self.story_state.rings.insert(ring.peer.clone(), ring.clone());
        }
        self.people_state.hub().set_story_rings(account, rings, cx);
        cx.notify();
    }

    /// Loads active story rings for all connected Telegram accounts (never marks anything seen).
    pub(super) fn load_stories_for_accounts(&mut self, cx: &mut Context<Self>) {
        let Some(snapshot) = &self.snapshot else { return };
        let accounts: Vec<AccountKey> = snapshot
            .accounts
            .iter()
            .filter(|a| a.key.network == protocol::Network::Telegram && a.link == protocol::Link::Connected)
            .map(|a| a.key.clone())
            .collect();
        for account in accounts {
            let ask = account.clone();
            cx.spawn(async move |this, cx| {
                let answer = cx
                    .background_executor()
                    .spawn(async move {
                        crate::client::ask(&Request::Stories {
                            account: ask,
                            peer_id: None,
                        })
                    })
                    .await;
                if let Ok(Response::Stories { rings, .. }) = answer {
                    let _ = this.update(cx, |shell, cx| {
                        shell.stories_changed(&account, &rings, cx);
                    });
                }
            })
            .detach();
        }
    }

    /// Returns the story ring for `peer` if they have any active stories.
    pub(in crate::gui) fn story_ring_for(&self, peer: &Key) -> Option<&PeerStoryRing> {
        self.story_state.rings.get(peer)
    }

    /// Opens the Story Viewer for `peer` (optionally jumping to `target_story_id`).
    /// Only once the peer's stories arrive and are displayed does it send `Request::ReadStory`.
    pub(in crate::gui) fn open_story_viewer(
        &mut self,
        peer: Key,
        target_story_id: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let account = peer.account();
        let author_name = self
            .story_state
            .rings
            .get(&peer)
            .map(|r| r.author_name.clone())
            .filter(|n| !n.is_empty())
            .or_else(|| self.people_state.hub().person(&peer).map(|p| p.name))
            .unwrap_or_else(|| peer.id.clone());
        let reply_input = cx.new(|cx| TextInput::new("Reply to story…", cx));
        self.story_state.viewer = Some(Viewer {
            account: account.clone(),
            peer: peer.clone(),
            author_name,
            stories: Vec::new(),
            index: 0,
            loading: true,
            error: None,
            reply_input,
            reply_sending: false,
            reply_status: None,
            started_at: Instant::now(),
            marked_read: HashSet::new(),
            timer: None,
        });
        cx.notify();

        let peer_id = peer.id.clone();
        let ask_account = account.clone();
        cx.spawn(async move |this, cx| {
            let answer = cx
                .background_executor()
                .spawn(async move {
                    crate::client::ask(&Request::Stories {
                        account: ask_account,
                        peer_id: Some(peer_id),
                    })
                })
                .await;
            let _ = this.update(cx, |shell, cx| {
                match answer {
                    Ok(Response::Stories { rings, stories }) => {
                        shell.stories_changed(&account, &rings, cx);
                        let mut should_mark: Option<(AccountKey, String, String)> = None;
                        if let Some(viewer) = shell.story_state.viewer.as_mut() {
                            if viewer.peer == peer {
                                viewer.loading = false;
                                if stories.is_empty() {
                                    viewer.error = Some("No active stories.".to_owned());
                                } else {
                                    let idx = initial_story_index(&stories, target_story_id.as_deref());
                                    if let Some(first) = stories.get(idx) {
                                        if !first.author_name.is_empty() {
                                            viewer.author_name = first.author_name.clone();
                                        }
                                        if !first.own && !first.seen && viewer.marked_read.insert(first.id.clone()) {
                                            should_mark = Some((
                                                viewer.account.clone(),
                                                viewer.peer.id.clone(),
                                                first.id.clone(),
                                            ));
                                        }
                                    }
                                    viewer.stories = stories;
                                    viewer.index = idx;
                                    viewer.started_at = Instant::now();
                                }
                            }
                        }
                        if let Some((acct, pid, sid)) = should_mark {
                            shell.send_read_story(acct, pid, sid, cx);
                        }
                        shell.fetch_current_story_media(cx);
                        shell.arm_story_timer(cx);
                    }
                    Ok(Response::Error { detail, .. }) => {
                        if let Some(viewer) = shell.story_state.viewer.as_mut() {
                            viewer.loading = false;
                            viewer.error = Some(detail);
                        }
                    }
                    Err(err) => {
                        if let Some(viewer) = shell.story_state.viewer.as_mut() {
                            viewer.loading = false;
                            viewer.error = Some(err.to_string());
                        }
                    }
                    _ => {}
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(in crate::gui) fn close_story_viewer(&mut self, cx: &mut Context<Self>) {
        self.story_state.viewer = None;
        cx.notify();
    }

    fn send_read_story(&self, account: AccountKey, peer_id: String, story_id: String, cx: &mut Context<Self>) {
        cx.spawn(async move |_, cx| {
            let _ = cx
                .background_executor()
                .spawn(async move {
                    crate::client::ask(&Request::ReadStory {
                        account,
                        peer_id,
                        story_id,
                    })
                })
                .await;
        })
        .detach();
    }

    fn fetch_current_story_media(&mut self, cx: &mut Context<Self>) {
        let want = self.story_state.viewer.as_ref().and_then(|v| {
            let story = v.current()?;
            let file = story.media.as_ref().or(story.thumb.as_ref())?;
            Some((v.account.clone(), file.clone()))
        });
        if let Some((account, file)) = want {
            let key = Key::new(&account, "");
            if self.media_missing(&key, &file) {
                self.want_media(vec![(key, file.id)], cx);
            }
        }
    }

    fn arm_story_timer(&mut self, cx: &mut Context<Self>) {
        let Some(viewer) = self.story_state.viewer.as_mut() else {
            return;
        };
        if viewer.loading || viewer.stories.is_empty() {
            viewer.timer = None;
            return;
        }
        let tick = Duration::from_millis(200);
        viewer.timer = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(tick).await;
                let keep_going = this
                    .update(cx, |shell, cx| {
                        let Some(viewer) = shell.story_state.viewer.as_mut() else {
                            return false;
                        };
                        // Pause auto-advance while the user is typing a reply.
                        let typing = !viewer.reply_input.read(cx).text().trim().is_empty() || viewer.reply_sending;
                        if typing {
                            viewer.started_at = Instant::now();
                            return true;
                        }
                        if viewer.progress() >= 1.0 {
                            shell.story_next(cx);
                            return shell.story_state.viewer.is_some();
                        }
                        cx.notify();
                        true
                    })
                    .unwrap_or(false);
                if !keep_going {
                    break;
                }
            }
        }));
    }

    fn story_next(&mut self, cx: &mut Context<Self>) {
        let mut should_mark = None;
        let advanced = if let Some(viewer) = self.story_state.viewer.as_mut() {
            let moved = viewer.next();
            if moved {
                let candidate = viewer
                    .current()
                    .filter(|cur| !cur.own && !cur.seen)
                    .map(|cur| cur.id.clone());
                if let Some(sid) = candidate {
                    if viewer.marked_read.insert(sid.clone()) {
                        should_mark = Some((viewer.account.clone(), viewer.peer.id.clone(), sid));
                    }
                }
            }
            moved
        } else {
            false
        };
        if !advanced {
            self.close_story_viewer(cx);
            return;
        }
        if let Some((acct, pid, sid)) = should_mark {
            self.send_read_story(acct, pid, sid, cx);
        }
        self.fetch_current_story_media(cx);
        cx.notify();
    }

    fn story_prev(&mut self, cx: &mut Context<Self>) {
        let mut should_mark = None;
        if let Some(viewer) = self.story_state.viewer.as_mut() {
            if viewer.prev() {
                let candidate = viewer
                    .current()
                    .filter(|cur| !cur.own && !cur.seen)
                    .map(|cur| cur.id.clone());
                if let Some(sid) = candidate {
                    if viewer.marked_read.insert(sid.clone()) {
                        should_mark = Some((viewer.account.clone(), viewer.peer.id.clone(), sid));
                    }
                }
            }
        }
        if let Some((acct, pid, sid)) = should_mark {
            self.send_read_story(acct, pid, sid, cx);
        }
        self.fetch_current_story_media(cx);
        cx.notify();
    }

    fn story_react(&mut self, emoji: String, cx: &mut Context<Self>) {
        let Some(viewer) = self.story_state.viewer.as_mut() else {
            return;
        };
        let account = viewer.account.clone();
        let peer_id = viewer.peer.id.clone();
        let Some(story) = viewer.current_mut() else {
            return;
        };
        let next = if story.my_reaction.as_deref() == Some(emoji.as_str()) {
            None
        } else {
            Some(emoji)
        };
        story.my_reaction = next.clone();
        let story_id = story.id.clone();
        cx.notify();

        cx.spawn(async move |_, cx| {
            let _ = cx
                .background_executor()
                .spawn(async move {
                    crate::client::ask(&Request::ReactStory {
                        account,
                        peer_id,
                        story_id,
                        emoji: next,
                    })
                })
                .await;
        })
        .detach();
    }

    fn story_send_reply(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(viewer) = self.story_state.viewer.as_mut() else {
            return;
        };
        if viewer.reply_sending {
            return;
        }
        let text = viewer.reply_input.read(cx).text().trim().to_owned();
        if text.is_empty() {
            return;
        }
        let Some(story) = viewer.current() else {
            return;
        };
        let account = viewer.account.clone();
        let peer_id = viewer.peer.id.clone();
        let story_id = story.id.clone();
        let client_id = format!("sr{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis());
        viewer.reply_sending = true;
        viewer.reply_status = None;
        let input = viewer.reply_input.clone();
        input.update(cx, |field, cx| field.clear(cx));
        cx.notify();

        cx.spawn(async move |this, cx| {
            let answer = cx
                .background_executor()
                .spawn(async move {
                    crate::client::ask(&Request::ReplyStory {
                        account,
                        peer_id,
                        story_id,
                        client_id,
                        text,
                    })
                })
                .await;
            let _ = this.update(cx, |shell, cx| {
                if let Some(viewer) = shell.story_state.viewer.as_mut() {
                    viewer.reply_sending = false;
                    viewer.reply_status = match answer {
                        Ok(Response::StoryReplied { .. }) => Some("Reply sent".to_owned()),
                        Ok(Response::Error { detail, .. }) => Some(detail),
                        Err(err) => Some(err.to_string()),
                        _ => Some("Reply failed".to_owned()),
                    };
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Renders the full-screen Story Viewer overlay when open.
    pub(in crate::gui) fn story_viewer_layer(&self, _window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let viewer = self.story_state.viewer.as_ref()?;
        let total = viewer.stories.len();
        let cur = viewer.current();
        let progress = viewer.progress();

        // Segmented progress bar across the top of the card.
        let bars = (0..total.max(1)).map(|i| {
            let fill = if i < viewer.index {
                1.0
            } else if i == viewer.index {
                progress
            } else {
                0.0
            };
            div()
                .flex_1()
                .h(px(3.))
                .rounded(px(2.))
                .bg(theme::border_strong())
                .overflow_hidden()
                .child(div().h_full().w(gpui::relative(fill)).bg(theme::accent()))
        });

        let header_meta = cur.map(|s| {
            let mut parts = vec![story_time_label(s)];
            if s.close_friends {
                parts.push("Close Friends".to_owned());
            }
            if let Some(views) = s.views_count {
                parts.push(format!("{views} views"));
            }
            parts.join(" · ")
        });

        // Resolve story media image path on disk if fetched.
        let media_path: Option<PathBuf> = cur.and_then(|s| {
            let file = s.media.as_ref().or(s.thumb.as_ref())?;
            self.media_path(&Key::new(&viewer.account, ""), Some(file))
                .or_else(|| file.path.as_ref().filter(|p| !p.is_empty()).map(PathBuf::from))
        });

        let media_box: AnyElement = if viewer.loading {
            div()
                .flex_1()
                .min_h(px(280.))
                .flex()
                .items_center()
                .justify_center()
                .type_ui()
                .text_color(theme::text_soft())
                .child("Loading story…")
                .into_any_element()
        } else if let Some(err) = &viewer.error {
            div()
                .flex_1()
                .min_h(px(280.))
                .flex()
                .items_center()
                .justify_center()
                .type_ui()
                .text_color(theme::text_soft())
                .child(err.clone())
                .into_any_element()
        } else if let Some(story) = cur {
            let mut stage = div()
                .flex_1()
                .min_h(px(300.))
                .relative()
                .rounded(px(theme::R_CONTROL))
                .bg(theme::raised())
                .overflow_hidden()
                .flex()
                .items_center()
                .justify_center();
            if let Some(path) = media_path {
                stage = stage.child(
                    img(path)
                        .max_w_full()
                        .max_h(px(440.))
                        .object_fit(gpui::ObjectFit::Contain),
                );
            } else {
                let kind_label = match story.media_kind {
                    StoryMediaKind::Photo => "Photo story",
                    StoryMediaKind::Video => "Video story",
                    StoryMediaKind::Unsupported => "Story",
                };
                stage = stage.child(
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap(px(6.))
                        .child(glyph("icons/play.svg", 24., theme::text_soft()))
                        .child(div().type_ui().text_color(theme::text_soft()).child(kind_label)),
                );
            }
            if story.media_kind == StoryMediaKind::Video {
                stage = stage.child(
                    div()
                        .absolute()
                        .top(px(8.))
                        .right(px(8.))
                        .px(px(6.))
                        .py(px(2.))
                        .rounded(px(theme::R_CONTROL))
                        .bg(theme::overlay())
                        .type_small()
                        .text_color(theme::text())
                        .child("Video"),
                );
            }
            stage.into_any_element()
        } else {
            div().flex_1().into_any_element()
        };

        let caption = cur
            .map(|s| s.caption.trim().to_owned())
            .filter(|c| !c.is_empty());
        let active_reaction = cur.and_then(|s| s.my_reaction.clone());

        let reactions = div().flex().items_center().gap(px(4.)).children(
            QUICK_REACTIONS.iter().enumerate().map(|(ix, &emoji)| {
                let selected = active_reaction.as_deref() == Some(emoji);
                let chosen = emoji.to_owned();
                div()
                    .id(("story-react", ix))
                    .cursor_pointer()
                    .px(px(8.))
                    .py(px(4.))
                    .rounded(px(theme::R_CONTROL))
                    .bg(if selected { theme::accent_wash() } else { theme::raised() })
                    .border_1()
                    .border_color(if selected { theme::accent() } else { theme::hairline() })
                    .type_ui()
                    .child(emoji)
                    .on_click(cx.listener(move |this, _, _, cx| this.story_react(chosen.clone(), cx)))
            }),
        );

        let reply_row = cur.is_some_and(|s| !s.own).then(|| {
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .px(px(10.))
                        .py(px(6.))
                        .rounded(px(theme::R_CONTROL))
                        .bg(theme::raised())
                        .border_1()
                        .border_color(theme::hairline())
                        .child(viewer.reply_input.clone()),
                )
                .child(
                    button("story-reply-send", ButtonKind::Primary, if viewer.reply_sending { "Sending…" } else { "Reply" })
                        .on_click(cx.listener(|this, _, window, cx| this.story_send_reply(window, cx))),
                )
        });

        let card = div()
            .id("story-viewer-modal")
            .w(px(420.))
            .max_w(px(420.))
            .max_h(px(660.))
            .p(px(14.))
            .rounded(px(theme::R_MODAL))
            .bg(theme::overlay())
            .border_1()
            .border_color(theme::overlay_edge())
            .shadow(theme::shadow_pane())
            .flex()
            .flex_col()
            .gap(px(10.))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            // Top segmented progress bar
            .child(div().flex().items_center().gap(px(4.)).children(bars))
            // Header: author, time/views, prev/next, close
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(6.))
                                    .child(div().type_ui().font_weight(theme::SEMIBOLD).truncate().child(viewer.author_name.clone()))
                                    .child(
                                        div()
                                            .px(px(5.))
                                            .py(px(1.))
                                            .rounded(px(theme::R_CONTROL))
                                            .bg(theme::accent_wash())
                                            .type_micro()
                                            .text_color(theme::accent_text())
                                            .child("Story"),
                                    ),
                            )
                            .children(header_meta.map(|m| div().type_small().text_color(theme::text_faint()).child(m))),
                    )
                    .when(total > 1, |row| {
                        row.child(
                            icon_button("story-prev", "icons/chevron-left.svg", false)
                                .aria_label("Previous story")
                                .on_click(cx.listener(|this, _, _, cx| this.story_prev(cx))),
                        )
                        .child(
                            icon_button("story-next", "icons/chevron-right.svg", false)
                                .aria_label("Next story")
                                .on_click(cx.listener(|this, _, _, cx| this.story_next(cx))),
                        )
                    })
                    .child(
                        icon_button("story-close", "icons/x.svg", false)
                            .aria_label("Close story viewer")
                            .on_click(cx.listener(|this, _, _, cx| this.close_story_viewer(cx))),
                    ),
            )
            .child(media_box)
            .children(caption.map(|text| div().type_ui().text_color(theme::text()).child(text)))
            .child(reactions)
            .children(reply_row)
            .children(
                viewer
                    .reply_status
                    .as_ref()
                    .map(|status| div().type_small().text_color(theme::text_soft()).child(status.clone())),
            );

        Some(
            div()
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .occlude()
                .child(scrim().on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| this.close_story_viewer(cx))))
                .child(card)
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::Network;

    fn sample_story(id: &str, seen: bool) -> Story {
        Story {
            id: id.into(),
            peer: Key {
                network: Network::Telegram,
                account: "7001".into(),
                id: "u7002".into(),
            },
            author_name: "Odalys".into(),
            posted_at: 1_700_000_000,
            expires_at: 1_700_086_400,
            seen,
            close_friends: false,
            pinned: false,
            own: false,
            noforwards: false,
            media_kind: StoryMediaKind::Photo,
            media: None,
            thumb: None,
            duration_ms: None,
            caption: format!("Story {id}"),
            caption_spans: vec![],
            my_reaction: None,
            views_count: None,
            reactions_count: None,
        }
    }

    #[test]
    fn initial_story_index_picks_target_then_first_unseen() {
        let stories = vec![
            sample_story("301", true),
            sample_story("302", false),
            sample_story("303", false),
        ];
        assert_eq!(initial_story_index(&stories, None), 1);
        assert_eq!(initial_story_index(&stories, Some("303")), 2);
        assert_eq!(initial_story_index(&stories, Some("999")), 1);

        let all_seen = vec![sample_story("301", true), sample_story("302", true)];
        assert_eq!(initial_story_index(&all_seen, None), 0);
    }

    #[test]
    fn unseen_ring_helper_filters_seen_peers() {
        let peer = Key {
            network: Network::Telegram,
            account: "7001".into(),
            id: "u7002".into(),
        };
        let mut rings = HashMap::new();
        rings.insert(
            peer.clone(),
            PeerStoryRing {
                peer: peer.clone(),
                author_name: "Odalys".into(),
                total: 2,
                unseen: 2,
                max_id: 302,
                max_read_id: 0,
                close_friends: true,
            },
        );
        assert!(unseen_ring(&rings, &peer).is_some());

        rings.get_mut(&peer).unwrap().unseen = 0;
        assert!(unseen_ring(&rings, &peer).is_none());
    }
}
