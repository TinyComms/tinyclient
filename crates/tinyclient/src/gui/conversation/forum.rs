//! A Discord forum's posts, in the main pane: one search page at a time.
//!
//! `show` loads when the open forum changes. Sorting, a tag, a search, and the page
//! controls each ask for one page of 25. The list stays up until that page arrives, and a
//! failure puts the offset back. The same forum redraws without asking again.

use crate::client::ask;
use crate::gui::components::{button, ButtonKind};
use crate::gui::input::TextInput;
use crate::gui::state::Shell;
use crate::gui::theme::{self, Type};
use gpui::{div, img, prelude::*, px, AnyElement, ClickEvent, Context, Entity, IntoElement, ObjectFit, SharedString, WeakEntity, Window};
use protocol::{Change, Conversation, ForumInfo, Key, MediaFile, Request, Response, TopicSort};
use std::collections::HashSet;
use std::path::PathBuf;

const PAGE: u32 = 25;
/// Longest side, in pixels, of a post thumbnail on a card or in the gallery.
const THUMB: u32 = 360;

pub(super) struct Board {
    shell: WeakEntity<Shell>,
    search: Entity<TextInput>,
    forum: Option<Key>,
    info: Option<ForumInfo>,
    posts: Vec<Conversation>,
    sort: TopicSort,
    tag: Option<String>,
    query: String,
    offset: u32,
    total: u32,
    loading: bool,
    error: String,
    epoch: u64,
    info_epoch: u64,
    /// Thumbnail ids already asked for. Cleared when the open forum changes.
    asked: HashSet<String>,
}

impl Board {
    pub(super) fn new(shell: WeakEntity<Shell>, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| TextInput::new("Search posts", cx));
        Self {
            shell,
            search,
            forum: None,
            info: None,
            posts: Vec::new(),
            sort: TopicSort::Activity,
            tag: None,
            query: String::new(),
            offset: 0,
            total: 0,
            loading: false,
            error: String::new(),
            epoch: 0,
            info_epoch: 0,
            asked: HashSet::new(),
        }
    }

    /// Draws `open`. A later frame of the same forum does not fetch again.
    pub(super) fn show(&mut self, open: &Conversation, cx: &mut Context<Self>) {
        if self.forum.as_ref() == Some(&open.key) {
            return;
        }
        self.forum = Some(open.key.clone());
        self.info = None;
        self.posts.clear();
        self.sort = TopicSort::Activity;
        self.tag = None;
        self.query.clear();
        self.offset = 0;
        self.total = 0;
        self.error.clear();
        self.asked.clear();
        self.search.update(cx, |input, cx| input.clear(cx));
        self.load_info(cx);
        self.load_at(0, cx);
    }

    fn set_sort(&mut self, sort: TopicSort, cx: &mut Context<Self>) {
        if self.sort == sort && self.offset == 0 {
            return;
        }
        self.sort = sort;
        self.load_at(0, cx);
    }

    fn toggle_tag(&mut self, id: String, cx: &mut Context<Self>) {
        self.tag = match self.tag.as_deref() {
            Some(current) if current == id => None,
            _ => Some(id),
        };
        self.load_at(0, cx);
    }

    fn run_search(&mut self, cx: &mut Context<Self>) {
        self.query = self.search.read(cx).text().trim().to_owned();
        self.load_at(0, cx);
    }

    fn older(&mut self, cx: &mut Context<Self>) {
        if self.loading {
            return;
        }
        self.load_at(self.offset.saturating_add(PAGE), cx);
    }

    fn newer(&mut self, cx: &mut Context<Self>) {
        if self.loading || self.offset == 0 {
            return;
        }
        self.load_at(self.offset.saturating_sub(PAGE), cx);
    }

    fn load_info(&mut self, cx: &mut Context<Self>) {
        let Some(forum) = self.forum.clone() else {
            return;
        };
        self.info_epoch = self.info_epoch.wrapping_add(1);
        let epoch = self.info_epoch;
        let request = Request::ForumInfo { forum };
        cx.spawn(async move |this, cx| {
            let answer = cx.background_executor().spawn(async move { ask(&request) }).await;
            let _ = this.update(cx, |this, cx| {
                if this.info_epoch != epoch {
                    return;
                }
                if let Ok(Response::ForumInfo { info }) = answer {
                    this.info = Some(info);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Asks for the page at `offset`. The list and `self.offset` stay until the page arrives.
    /// A failure puts the offset back and leaves the posts that are already on screen.
    fn load_at(&mut self, offset: u32, cx: &mut Context<Self>) {
        let Some(forum) = self.forum.clone() else {
            return;
        };
        let previous = self.offset;
        self.epoch = self.epoch.wrapping_add(1);
        let epoch = self.epoch;
        self.loading = true;
        self.error.clear();
        let request = Request::Topics {
            forum,
            offset,
            limit: PAGE,
            landing: false,
            sort: self.sort,
            tag: self.tag.clone(),
            query: (!self.query.is_empty()).then(|| self.query.clone()),
        };
        cx.spawn(async move |this, cx| {
            let answer = cx.background_executor().spawn(async move { ask(&request) }).await;
            let _ = this.update(cx, |this, cx| {
                if this.epoch != epoch {
                    return;
                }
                this.loading = false;
                match answer {
                    Ok(Response::Conversations { conversations, total, .. }) => {
                        this.posts = conversations;
                        this.total = total;
                        this.offset = offset;
                        this.error.clear();
                        this.fetch_thumbs(cx);
                    }
                    Ok(Response::Error { detail, .. }) => {
                        this.offset = previous;
                        this.error = detail;
                    }
                    _ => {
                        this.offset = previous;
                        this.error = "Couldn't load posts.".to_owned();
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Asks for post pictures that are not on disk yet. One ask per id for this forum.
    fn fetch_thumbs(&mut self, cx: &mut Context<Self>) {
        let Some(forum) = self.forum.clone() else {
            return;
        };
        let mut ids = Vec::new();
        for post in &self.posts {
            let Some(file) = post.thumbnail.as_ref() else { continue };
            let ready = file.failed.is_some() || file.path.as_ref().is_some_and(|path| !path.is_empty());
            if ready || self.asked.contains(&file.id) {
                continue;
            }
            ids.push(file.id.clone());
        }
        if ids.is_empty() {
            return;
        }
        for id in &ids {
            self.asked.insert(id.clone());
        }
        let forum_id = forum.id.clone();
        let request = Request::Fetch { retry: false, size: Some(THUMB), account: forum.account(), ids: ids.clone() };
        cx.spawn(async move |this, cx| {
            let answer = cx.background_executor().spawn(async move { ask(&request) }).await;
            let _ = this.update(cx, |this, cx| {
                if this.forum.as_ref().map(|key| key.id.as_str()) != Some(forum_id.as_str()) {
                    return;
                }
                match answer {
                    Ok(Response::Media { files }) => {
                        if this.take_paths(&files) {
                            cx.notify();
                        }
                    }
                    _ => {
                        for id in &ids {
                            this.asked.remove(id);
                        }
                    }
                }
            });
        })
        .detach();
    }

    /// A picture landed for a post on screen. The shell hears the same change and keeps its own copy.
    pub(super) fn media_changed(&mut self, change: &Change, cx: &mut Context<Self>) {
        let Change::Media { account, file } = change else {
            return;
        };
        if self.forum.as_ref().map(Key::account) != Some(account.clone()) {
            return;
        }
        if self.take_paths(std::slice::from_ref(file)) {
            cx.notify();
        }
    }

    /// Copies a landed path onto the post that shows that file. Returns whether any card changed.
    fn take_paths(&mut self, files: &[MediaFile]) -> bool {
        let mut changed = false;
        for file in files {
            let Some(path) = file.path.as_ref().filter(|path| !path.is_empty()) else {
                continue;
            };
            for post in &mut self.posts {
                let Some(thumb) = post.thumbnail.as_mut() else {
                    continue;
                };
                if thumb.id == file.id && thumb.path.as_deref() != Some(path.as_str()) {
                    thumb.path = Some(path.clone());
                    changed = true;
                }
            }
        }
        changed
    }

    fn tag_label(&self, id: &str) -> String {
        let Some(tag) = self.info.as_ref().and_then(|info| info.tags.iter().find(|tag| tag.id == id)) else {
            return id.to_owned();
        };
        if tag.emoji.is_empty() {
            tag.name.clone()
        } else {
            format!("{} {}", tag.emoji, tag.name)
        }
    }

    fn chip(&self, id: SharedString, label: String, on: bool, cx: &mut Context<Self>, toggle: impl Fn(&mut Self, &mut Context<Self>) + 'static) -> AnyElement {
        div()
            .id(id)
            .cursor_pointer()
            .px(px(8.))
            .h(px(24.))
            .rounded(px(theme::R_CHIP))
            .flex()
            .items_center()
            .type_ui()
            .bg(if on { theme::accent_soft() } else { theme::hover() })
            .text_color(if on { theme::text() } else { theme::text_faint() })
            .child(label)
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| toggle(this, cx)))
            .into_any_element()
    }

    fn post_card(&self, post: &Conversation, gallery: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let key = post.key.clone();
        let title = post.title.clone();
        let shell = self.shell.clone();
        let tags: Vec<String> = post.tags.iter().map(|id| self.tag_label(id)).collect();
        let path = post.thumbnail.as_ref().and_then(|file| file.path.as_deref()).filter(|path| !path.is_empty()).map(PathBuf::from);
        let tile = (gallery || post.thumbnail.is_some()).then(|| {
            let wide = if gallery { 180. } else { 72. };
            let high = if gallery { 120. } else { 48. };
            let picture = path.map(|path| img(path).w(px(wide)).h(px(high)).object_fit(ObjectFit::Cover));
            div()
                .w(px(wide))
                .h(px(high))
                .flex_none()
                .overflow_hidden()
                .rounded(px(theme::R_CARD))
                .bg(if post.thumbnail.is_some() { theme::accent_soft() } else { theme::hover() })
                .children(picture)
        });
        let mut body = div().min_w_0().flex().flex_col().gap(px(4.));
        body = body.child(div().type_ui().font_weight(theme::MEDIUM).text_color(theme::text()).child(post.title.clone()));
        if !tags.is_empty() {
            let chips = tags.into_iter().enumerate().map(|(ix, label)| {
                div()
                    .px(px(6.))
                    .h(px(20.))
                    .rounded(px(theme::R_CHIP))
                    .flex()
                    .items_center()
                    .bg(theme::hover())
                    .type_ui()
                    .text_color(theme::text())
                    .child(label)
                    .id(SharedString::from(format!("forum-card-tag-{}-{ix}", post.key.id)))
            });
            body = body.child(div().flex().flex_wrap().gap(px(4.)).children(chips));
        }
        if !post.preview.is_empty() {
            body = body.child(div().type_ui().text_color(theme::text_faint()).child(post.preview.clone()));
        }
        let counts = format!("{} messages · {} sent", post.message_count, post.total_message_sent);
        let mut meta = counts;
        if !post.reaction.is_empty() {
            meta.push_str(" · ");
            meta.push_str(&post.reaction);
        }
        if !post.owner.is_empty() {
            meta.push_str(" · ");
            meta.push_str(&post.owner);
        }
        body = body.child(div().type_ui().text_color(theme::text_faint()).child(meta));
        let card = div()
            .id(SharedString::from(format!("forum-post-{}", post.key.id)))
            .cursor_pointer()
            .p(px(10.))
            .rounded(px(theme::R_CARD))
            .bg(theme::raised())
            .gap(px(8.))
            .flex();
        let card = if gallery { card.w(px(200.)).flex_col() } else { card.w_full() };
        card.children(tile).child(body).on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
            let _ = shell.update(cx, |shell, cx| shell.select(key.clone(), &title, cx));
        }))
    }
}

impl Render for Board {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let gallery = self.info.as_ref().is_some_and(|info| info.media);
        let activity = self.chip("forum-sort-activity".into(), "Activity".to_owned(), self.sort == TopicSort::Activity, cx, |this, cx| {
            this.set_sort(TopicSort::Activity, cx);
        });
        let created = self.chip("forum-sort-created".into(), "Created".to_owned(), self.sort == TopicSort::Created, cx, |this, cx| {
            this.set_sort(TopicSort::Created, cx);
        });
        let tags = self.info.clone().map(|info| info.tags).unwrap_or_default();
        let selected = self.tag.clone();
        let tag_chips: Vec<_> = tags
            .into_iter()
            .map(|tag| {
                let id = tag.id.clone();
                let on = selected.as_deref() == Some(tag.id.as_str());
                let label = if tag.emoji.is_empty() { tag.name } else { format!("{} {}", tag.emoji, tag.name) };
                self.chip(SharedString::from(format!("forum-tag-{id}")), label, on, cx, move |this, cx| this.toggle_tag(id.clone(), cx))
            })
            .collect();
        let guidelines = self.info.as_ref().filter(|info| !info.guidelines.is_empty()).map(|info| {
            div().type_ui().text_color(theme::text_faint()).child(info.guidelines.clone())
        });
        let more = self.total > self.offset.saturating_add(self.posts.len() as u32);
        let posts = self.posts.clone();
        let cards: Vec<_> = posts.iter().map(|post| self.post_card(post, gallery, cx)).collect();
        let mut list = div().flex().gap(px(8.));
        list = if gallery { list.flex_wrap() } else { list.flex_col() };
        list = list.children(cards);
        let body = if posts.is_empty() && !self.error.is_empty() {
            div().type_ui().text_color(theme::danger()).child(self.error.clone())
        } else if posts.is_empty() && self.loading {
            div().type_ui().text_color(theme::text_faint()).child("Loading posts…")
        } else if posts.is_empty() {
            div().type_ui().text_color(theme::text_faint()).child("No posts")
        } else {
            let note = if !self.error.is_empty() {
                Some(div().type_ui().text_color(theme::danger()).child(self.error.clone()))
            } else if self.loading {
                Some(div().type_ui().text_color(theme::text_faint()).child("Loading posts…"))
            } else {
                None
            };
            let newer = (self.offset > 0).then(|| {
                button("forum-newer", ButtonKind::Secondary, "Newer posts").on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.newer(cx)))
            });
            let older = more.then(|| {
                button("forum-older", ButtonKind::Secondary, "Older posts").on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.older(cx)))
            });
            let paging = (newer.is_some() || older.is_some()).then(|| div().flex().gap(px(8.)).children(newer).children(older));
            div().flex().flex_col().gap(px(8.)).children(note).child(list).children(paging)
        };
        div()
            .id("forum-board")
            .size_full()
            .flex()
            .flex_col()
            .bg(theme::canvas())
            .child(
                div()
                    .flex_none()
                    .p(px(theme::PANE_PAD))
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .border_b_1()
                    .border_color(theme::hairline())
                    .child(div().flex().flex_wrap().items_center().gap(px(6.)).child(activity).child(created).children(tag_chips))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(div().flex_1().min_w_0().child(self.search.clone()))
                            .child(
                                button("forum-search", ButtonKind::Secondary, "Search")
                                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.run_search(cx))),
                            ),
                    )
                    .children(guidelines),
            )
            .child(
                div()
                    .id("forum-post-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p(px(theme::PANE_PAD))
                    .child(body),
            )
    }
}
