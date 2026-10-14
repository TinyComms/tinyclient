//! Shared: Media · Files · Links · Pinned of one conversation. A list from the cache says so;
//! a count is drawn only when the network gave a total; "Search all" asks the network once,
//! on a click, never by itself.

use crate::gui::components::{button_with_icon, glyph, Avatar, ButtonKind};
use crate::gui::people::Hub;
use crate::gui::theme::{self, Type};
use gpui::{div, prelude::*, px, App, Div, Entity};
use protocol::{Key, Message, Network, Request, Response, SharedItem, SharedKind};

/// Items read at once. The conversation has the rest.
const LIMIT: u32 = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Kind(SharedKind),
    Pinned,
}

/// What one tab holds, and how much of the whole that is.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Page {
    pub items: Vec<SharedItem>,
    pub pinned: Vec<Message>,
    pub more: bool,
    /// The network's own whole list (a search ran, or the network keeps one).
    pub complete: bool,
    pub total: Option<u32>,
    /// An answer came; until then nothing is claimed.
    pub read: bool,
}

/// The source line under the tabs, and what ends the list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub line: String,
    pub search: bool,
    /// Teams: the rest lives in the official app.
    pub open_in: bool,
}

fn none_words(tab: Tab) -> &'static str {
    match tab {
        Tab::Kind(SharedKind::Media) => "No photos or videos in this chat",
        Tab::Kind(SharedKind::Files) => "No files in this chat",
        Tab::Kind(SharedKind::Links) => "No links in this chat",
        Tab::Pinned => "Nothing pinned in this chat",
    }
}

/// The honesty rule, decided from the answer alone.
pub fn source(network: Network, tab: Tab, page: &Page) -> Source {
    let count = if tab == Tab::Pinned { page.pinned.len() } else { page.items.len() } as u32;
    let net = network.label();
    // Pinned is the network's own list, asked of it.
    if page.complete || tab == Tab::Pinned {
        let line = match (count, page.total.unwrap_or(count)) {
            (0, _) => none_words(tab).to_owned(),
            (_, total) => format!("All on {net} · {total}"),
        };
        return Source { line, search: false, open_in: false };
    }
    let line = match count {
        0 => "None in the messages loaded here".to_owned(),
        count => format!("From messages loaded here · {count}"),
    };
    // CONTRACT: Teams has no confirmed route to a chat's files, so its list is never searched.
    match network {
        Network::Teams => Source { line, search: false, open_in: true },
        _ => Source { line, search: true, open_in: false },
    }
}

/// A count on a tab only when the network gave an exact total.
pub fn tab_count(page: &Page) -> Option<u32> {
    page.total
}

/// The tabs a network has: Pinned is hidden on Teams.
pub fn tabs(network: Network) -> Vec<Tab> {
    let mut tabs = vec![Tab::Kind(SharedKind::Media), Tab::Kind(SharedKind::Files), Tab::Kind(SharedKind::Links)];
    if network != Network::Teams {
        tabs.push(Tab::Pinned);
    }
    tabs
}

/// The network keeps a whole, counted list of what was shared and gives it in one request,
/// so it is asked at once rather than the cache: Telegram (`messages.search` with its count).
pub fn whole_at_open(network: Network) -> bool {
    network == Network::Telegram
}

/// One conversation's Shared, held by the pane that draws it.
pub struct Shared {
    pub conversation: Option<Key>,
    pub tab: Tab,
    pub page: Page,
    /// Exact totals the network gave, by tab, for this conversation.
    pub totals: Vec<(Tab, u32)>,
    asked: u64,
}

impl Default for Shared {
    fn default() -> Shared {
        Shared { conversation: None, tab: Tab::Kind(SharedKind::Media), page: Page::default(), totals: Vec::new(), asked: 0 }
    }
}

impl Shared {
    /// Lists this conversation, from the cache. The same one again keeps what is held.
    pub fn show<V: 'static>(&mut self, conversation: Option<Key>, entity: &Entity<V>, cx: &mut App, held: impl Fn(&mut V) -> &mut Shared + Copy + 'static) {
        if conversation == self.conversation {
            return;
        }
        self.conversation = conversation;
        self.tab = Tab::Kind(SharedKind::Media);
        self.totals.clear();
        self.ask(false, entity, cx, held);
        // Where the network counts it all, every tab's count is asked with the open one.
        if let Some(conversation) = self.conversation.clone().filter(|key| whole_at_open(key.network)) {
            for kind in [SharedKind::Files, SharedKind::Links] {
                self.count(conversation.clone(), kind, entity, cx, held);
            }
        }
    }

    /// One tab's exact total, from the network's own count; the list waits for its tab.
    fn count<V: 'static>(&self, conversation: Key, kind: SharedKind, entity: &Entity<V>, cx: &mut App, held: impl Fn(&mut V) -> &mut Shared + 'static) {
        let weak = entity.downgrade();
        let request = Request::Shared { conversation: conversation.clone(), kind, before: None, limit: 1, search: true };
        crate::gui::people::ask_later(cx, request, move |answer, cx| {
            let Ok(Response::Shared { total: Some(total), .. }) = answer else {
                return;
            };
            let _ = weak.update(cx, |view, cx| {
                let shared = held(view);
                if shared.conversation.as_ref() == Some(&conversation) {
                    shared.told(Tab::Kind(kind), total);
                    cx.notify();
                }
            });
        });
    }

    fn told(&mut self, tab: Tab, total: u32) {
        self.totals.retain(|(known, _)| *known != tab);
        self.totals.push((tab, total));
    }

    /// The exact count a tab carries, when the network gave one.
    pub fn total_of(&self, tab: Tab) -> Option<u32> {
        if tab == self.tab {
            if let Some(total) = tab_count(&self.page) {
                return Some(total);
            }
        }
        self.totals.iter().find(|(known, _)| *known == tab).map(|(_, total)| *total)
    }

    pub fn pick<V: 'static>(&mut self, tab: Tab, entity: &Entity<V>, cx: &mut App, held: impl Fn(&mut V) -> &mut Shared + Copy + 'static) {
        if self.tab != tab {
            self.tab = tab;
            self.ask(false, entity, cx, held);
        }
    }

    /// `search`: the user clicked "Search all"; the network is asked. A network that gives its
    /// whole list at open is asked then too, and the cache answers if it cannot.
    pub fn ask<V: 'static>(&mut self, search: bool, entity: &Entity<V>, cx: &mut App, held: impl Fn(&mut V) -> &mut Shared + Copy + 'static) {
        self.asked += 1;
        if !search {
            self.page = Page::default();
        }
        let Some(conversation) = self.conversation.clone() else {
            return;
        };
        let whole = !search && whole_at_open(conversation.network);
        let (asked, tab, weak) = (self.asked, self.tab, entity.downgrade());
        let request = match tab {
            Tab::Kind(kind) => Request::Shared { conversation, kind, before: None, limit: LIMIT, search: search || whole },
            Tab::Pinned => Request::Pinned { conversation },
        };
        crate::gui::people::ask_later(cx, request, move |answer, cx| {
            let page = match answer {
                Ok(Response::Shared { items, more, complete, total, .. }) => {
                    Page { items, pinned: Vec::new(), more, complete, total, read: true }
                }
                Ok(Response::Pinned { messages, .. }) => {
                    let total = Some(messages.len() as u32);
                    Page { items: Vec::new(), pinned: messages, more: false, complete: true, total, read: true }
                }
                // The network could not say: what the cache holds, labelled as such.
                _ if whole => {
                    let _ = weak.update(cx, |view, cx| {
                        let entity = cx.entity();
                        let shared = held(view);
                        if shared.asked == asked && shared.tab == tab {
                            shared.ask_cache(&entity, cx, held);
                        }
                    });
                    return;
                }
                // Nothing claimed: the honest empty line, from the cache.
                _ => Page { read: true, ..Page::default() },
            };
            let _ = weak.update(cx, |view, cx| {
                let shared = held(view);
                if shared.asked == asked && shared.tab == tab {
                    if let Some(total) = page.total {
                        shared.told(tab, total);
                    }
                    shared.page = page;
                    cx.notify();
                }
            });
        });
    }

    /// The cache only, for a network that could not give its whole list.
    fn ask_cache<V: 'static>(&mut self, entity: &Entity<V>, cx: &mut App, held: impl Fn(&mut V) -> &mut Shared + 'static) {
        self.asked += 1;
        let (Some(conversation), Tab::Kind(kind)) = (self.conversation.clone(), self.tab) else {
            return;
        };
        let (asked, tab, weak) = (self.asked, self.tab, entity.downgrade());
        let request = Request::Shared { conversation, kind, before: None, limit: LIMIT, search: false };
        crate::gui::people::ask_later(cx, request, move |answer, cx| {
            let page = match answer {
                Ok(Response::Shared { items, more, complete, total, .. }) => Page { items, pinned: Vec::new(), more, complete, total, read: true },
                _ => Page { read: true, ..Page::default() },
            };
            let _ = weak.update(cx, |view, cx| {
                let shared = held(view);
                if shared.asked == asked && shared.tab == tab {
                    shared.page = page;
                    cx.notify();
                }
            });
        });
    }
}

/// `184 KB`
pub fn size_words(bytes: u64) -> String {
    match bytes {
        0 => String::new(),
        bytes if bytes < 1024 => format!("{bytes} B"),
        bytes if bytes < 1024 * 1024 => format!("{} KB", bytes.div_ceil(1024)),
        bytes => format!("{:.1} MB", bytes as f64 / (1024. * 1024.)),
    }
}

/// What a link reads as with no title: its host and path.
pub fn link_words(url: &str) -> &str {
    url.trim_start_matches("https://").trim_start_matches("http://").trim_end_matches('/')
}

fn host(url: &str) -> &str {
    link_words(url).split('/').next().unwrap_or("")
}

/// `0:14`
fn duration(seconds: u32) -> String {
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

fn extension(name: &str) -> String {
    name.rsplit_once('.').map(|(_, ext)| ext.to_uppercase()).filter(|ext| ext.len() <= 4).unwrap_or_else(|| "FILE".to_owned())
}

pub type Hook = Box<dyn Fn(&mut gpui::Window, &mut App)>;
pub type PickHook = Box<dyn Fn(Tab, &mut gpui::Window, &mut App)>;

/// Callbacks the drawing hands back to the pane.
pub struct Hooks {
    pub pick: PickHook,
    pub search: Hook,
    pub open_in: Option<Hook>,
}

/// The tab bar, the source line and the list. `width` is the content width, for the tiles.
pub fn draw(shared: &Shared, network: Network, hub: &Hub, width: f32, hooks: Hooks, cx: &mut App) -> Div {
    let Hooks { pick, search, open_in } = hooks;
    let pick = std::rc::Rc::new(pick);
    let bar = div().h(px(32.)).px(px(theme::PANE_PAD)).flex().items_end().gap(px(16.)).border_b_1().border_color(theme::hairline()).children(
        tabs(network).into_iter().enumerate().map(|(ix, tab)| {
            let on = shared.tab == tab;
            let words = match tab {
                Tab::Kind(SharedKind::Media) => "Media",
                Tab::Kind(SharedKind::Files) => "Files",
                Tab::Kind(SharedKind::Links) => "Links",
                Tab::Pinned => "Pinned",
            };
            let count = shared.total_of(tab);
            let pick = pick.clone();
            div()
                .id(("shared-tab", ix))
                .h(px(32.))
                .flex()
                .items_center()
                .gap(px(4.))
                .cursor_pointer()
                .type_ui()
                .text_color(if on { theme::text() } else { theme::text_soft() })
                .when(on, |tab| tab.border_b_2().border_color(theme::text()))
                .hover(|tab| tab.text_color(theme::text()))
                .child(words)
                .children(count.map(|count| div().type_micro().text_color(theme::text_faint()).child(super::facts::thousands(count))))
                .on_click(move |_, window, cx| pick(tab, window, cx))
        }),
    );
    let page = &shared.page;
    let mut body = div().px(px(theme::PANE_PAD)).pt(px(8.)).flex().flex_col();
    if page.read {
        let told = source(network, shared.tab, page);
        body = body.child(div().pb(px(8.)).type_small().text_color(theme::text_faint()).child(told.line.clone()));
        body = body.child(list(shared, hub, width, cx));
        if told.search {
            body = body.child(
                button_with_icon("shared-search", ButtonKind::Secondary, "icons/search.svg", format!("Search all on {}", network.label()))
                    .mt(px(8.))
                    .w_full()
                    .on_click(move |_, window, cx| search(window, cx)),
            );
        }
        if told.open_in {
            body = body.child(div().pt(px(8.)).type_small().text_color(theme::text_soft()).child(format!("The rest is in {}.", network.label())));
        }
        // Only where nothing else on the pane opens the network's app.
        if let (true, Some(open_in)) = (told.open_in, open_in) {
            body = body
                .child(
                    crate::gui::components::button("shared-open", ButtonKind::Secondary, format!("Open in {}", network.label()))
                        .mt(px(8.))
                        .w_full()
                        .on_click(move |_, window, cx| open_in(window, cx)),
                );
        }
    }
    div().flex().flex_col().child(bar).child(body)
}

fn list(shared: &Shared, hub: &Hub, width: f32, cx: &mut App) -> Div {
    let Some(conversation) = &shared.conversation else {
        return div();
    };
    let account = conversation.account();
    let page = &shared.page;
    match shared.tab {
        Tab::Kind(SharedKind::Media) => {
            // Three to a row, each a third of the row, whatever the pane's width; the height
            // follows the width the pane says it has.
            let tile = ((width - 4.) / 3.).floor().max(40.);
            let tiles: Vec<_> = page.items.iter().enumerate().filter_map(|(ix, item)| {
                let attachment = item.attachment.as_ref()?;
                let shown = attachment.thumb.as_ref().unwrap_or(&attachment.file);
                let picture = hub.file(&account, shown, cx);
                let on_disk = attachment.file.path.clone();
                Some(
                    div()
                        .id(("shared-tile", ix))
                        .relative()
                        .flex_1()
                        .min_w_0()
                        .h(px(tile))
                        .overflow_hidden()
                        .cursor_pointer()
                        .bg(theme::raised())
                        .children(picture.path.clone().map(|path| {
                            gpui::img(std::path::PathBuf::from(path)).size_full().object_fit(gpui::ObjectFit::Cover)
                        }))
                        .children((attachment.duration > 0).then(|| {
                            div()
                                .absolute()
                                .left(px(4.))
                                .bottom(px(4.))
                                .px(px(4.))
                                .rounded(px(3.))
                                .bg(gpui::rgba(0x00000099))
                                .type_micro()
                                .text_color(theme::text())
                                .child(duration(attachment.duration))
                        }))
                        // CONTRACT: the media viewer (design 03) has no entry point the pane can
                        // name, so a tile opens the file itself.
                        .on_click(move |_, _, cx| {
                            if let Some(path) = &on_disk {
                                cx.open_url(&format!("file://{path}"));
                            }
                        }),
                )
            }).collect();
            let mut grid = div().rounded(px(theme::R_CARD)).overflow_hidden().flex().flex_col().gap(px(2.));
            let mut tiles = tiles.into_iter().peekable();
            while tiles.peek().is_some() {
                let row: Vec<_> = tiles.by_ref().take(3).map(IntoElement::into_any_element).collect();
                let short = 3 - row.len();
                grid = grid.child(div().flex().gap(px(2.)).children(row).children((0..short).map(|_| div().flex_1())));
            }
            grid
        }
        Tab::Kind(SharedKind::Files) => div().flex().flex_col().children(page.items.iter().enumerate().filter_map(|(ix, item)| {
            let attachment = item.attachment.as_ref()?;
            let on_disk = attachment.file.path.clone();
            let said = [size_words(attachment.file.bytes), item.sender.clone(), crate::gui::time::stamp(crate::view::now(), item.sent)];
            let said = said.into_iter().filter(|part| !part.is_empty()).collect::<Vec<_>>().join(" · ");
            Some(
                two_line(("shared-file", ix), tile_words(extension(&attachment.name)), attachment.name.clone(), said)
                    .on_click(move |_, _, cx| {
                        if let Some(path) = &on_disk {
                            cx.open_url(&format!("file://{path}"));
                        }
                    }),
            )
        })),
        Tab::Kind(SharedKind::Links) => div().flex().flex_col().children(page.items.iter().enumerate().map(|(ix, item)| {
            let url = item.url.clone();
            let title = if item.title.is_empty() { link_words(&item.url).to_owned() } else { item.title.clone() };
            two_line(("shared-link", ix), div().child(glyph("icons/link.svg", 16., theme::text_soft())), title, host(&item.url).to_owned())
                .on_click(move |_, _, cx| cx.open_url(&url))
        })),
        Tab::Pinned => div().flex().flex_col().gap(px(8.)).children(page.pinned.iter().map(|message| {
            let author = message.sender.clone();
            div()
                .flex()
                .items_start()
                .gap(px(8.))
                .child(Avatar::named(&author.name, 20.).build())
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(div().type_small().font_weight(theme::SEMIBOLD).child(author.name))
                        .child(div().type_ui().line_clamp(2).child(message.text.clone())),
                )
        })),
    }
}

fn tile_words(words: String) -> Div {
    div().text_size(px(11.)).line_height(px(14.)).font_weight(theme::SEMIBOLD).text_color(theme::text_soft()).child(words)
}

fn two_line(id: impl Into<gpui::ElementId>, lead: Div, title: String, said: String) -> gpui::Stateful<Div> {
    div()
        .id(id)
        .h(px(theme::ROW_TWO))
        .mx(px(-8.))
        .px(px(8.))
        .rounded(px(theme::R_CONTROL))
        .flex()
        .items_center()
        .gap(px(10.))
        .cursor_pointer()
        .hover(|row| row.bg(theme::hover()))
        .child(div().flex_none().size(px(32.)).rounded(px(theme::R_CONTROL)).bg(theme::raised()).flex().items_center().justify_center().child(lead))
        .child(
            div()
                .min_w_0()
                .flex()
                .flex_col()
                .child(div().truncate().type_ui().child(title))
                .child(div().truncate().type_small().text_color(theme::text_soft()).child(said)),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item() -> SharedItem {
        SharedItem { message_id: "m".into(), sent: 0, sender: "Ines".into(), attachment: None, url: "https://maps.example.org/ridge/".into(), title: String::new() }
    }

    #[test]
    fn a_list_from_the_cache_says_so_and_offers_the_search() {
        let media = Tab::Kind(SharedKind::Media);
        let empty = Page { read: true, ..Page::default() };
        assert_eq!(source(Network::Discord, media, &empty), Source { line: "None in the messages loaded here".into(), search: true, open_in: false });
        let cached = Page { items: vec![item(), item()], read: true, ..Page::default() };
        assert_eq!(source(Network::Discord, media, &cached).line, "From messages loaded here · 2");
        // No count on the tab: the cache counted nothing on the network.
        assert_eq!(tab_count(&cached), None);
        // Teams hands the rest off instead of searching.
        let teams = source(Network::Teams, media, &cached);
        assert!(!teams.search && teams.open_in);
    }

    #[test]
    fn a_complete_answer_counts_from_its_total_only() {
        let links = Tab::Kind(SharedKind::Links);
        let whole = Page { items: vec![item()], complete: true, total: Some(37), read: true, ..Page::default() };
        assert_eq!(source(Network::Telegram, links, &whole), Source { line: "All on Telegram · 37".into(), search: false, open_in: false });
        assert_eq!(tab_count(&whole), Some(37));
        let none = Page { complete: true, total: Some(0), read: true, ..Page::default() };
        assert_eq!(source(Network::Telegram, links, &none).line, "No links in this chat");
        assert_eq!(tabs(Network::Teams).len(), 3);
        assert_eq!(tabs(Network::Slack).last(), Some(&Tab::Pinned));
    }

    #[test]
    fn telegram_gives_its_whole_counted_list_at_open_and_every_tab_keeps_its_count() {
        assert!(whole_at_open(Network::Telegram) && !whole_at_open(Network::Discord) && !whole_at_open(Network::Teams));
        let mut shared = Shared::default();
        shared.told(Tab::Kind(SharedKind::Files), 3);
        shared.told(Tab::Kind(SharedKind::Files), 4);
        assert_eq!(shared.total_of(Tab::Kind(SharedKind::Files)), Some(4));
        assert_eq!(shared.total_of(Tab::Kind(SharedKind::Links)), None);
        shared.page = Page { complete: true, total: Some(9), read: true, ..Page::default() };
        assert_eq!(shared.total_of(Tab::Kind(SharedKind::Media)), Some(9));
    }

    #[test]
    fn sizes_links_and_lengths_read_as_a_person_says_them() {
        assert_eq!(size_words(188_416), "184 KB");
        assert_eq!(size_words(19_293_798), "18.4 MB");
        assert_eq!(link_words("https://example.org/wiki/Lepraria/"), "example.org/wiki/Lepraria");
        assert_eq!(host("https://maps.example.org/ridge"), "maps.example.org");
        assert_eq!(duration(74), "1:14");
        assert_eq!(extension("pager-runbook-v4.pdf"), "PDF");
        assert_eq!(extension("README"), "FILE");
    }
}
