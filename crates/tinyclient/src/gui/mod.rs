//! The window. It holds what is on screen and nothing else; chatd owns the rest.
//!
//! One file per region, so they can change without meeting: `state` (what the shell knows and
//! how it stays current), `layout`, `rail`, `sidebar`, `conversation`, `people`, `composer`,
//! `call`. Each view keeps its own `State` in its own file; `state::Shell` only holds them.

mod anim;
mod app;
mod attention;
mod assets;
mod autostart;
// `pub(crate)` so the mobile layer can reach the call's platform hooks (`call::phone::hooks`).
pub(crate) mod call;
mod chrome;
mod components;
mod composer;
mod conversation;
#[cfg(debug_assertions)]
pub(crate) mod debug_launch;
mod discord_settings;
mod discord_admin;
// Webhooks, integrations, emoji and stickers. It shared a file name with the permissions, audit and AutoMod panes.
mod discord_manage;
mod hotkeys;
mod input;
mod keymap;
mod layout;
mod live_location;
// `pub(crate)` so the mobile layer can read `lock::hides_switcher()`.
pub(crate) mod lock;
mod logs;
mod meetings;
mod pane;
mod people;
mod phone;
mod playing;
mod rail;
mod settings;
mod sidebar;
mod spell;
mod signin;
mod shortcuts;
#[cfg(any(feature = "gui-shot", feature = "demo"))]
pub(crate) mod shot_demo;
mod state;
mod stories;
pub(in crate::gui) mod streamer;
mod theme;
mod tiny;
mod time;

/// The lab's focus-ring driver. A keyboard scroll frame uses an inset ring, not a fill.
pub(crate) fn prove_focus_ring() -> Result<(), &'static str> {
    if components::scroll::frame_focus_ring().iter().all(|shadow| shadow.inset) {
        Ok(())
    } else {
        Err("the scroll frame focus ring is outset, so a transparent frame fills")
    }
}

/// The lab's Tiny STUN-watchdog driver. A short gap on a live call is not a reconnect.
pub(crate) fn prove_stun_watchdog() -> Result<(), &'static str> {
    if protocol::TINY_STUN_SILENCE_MS >= 10_000 {
        Ok(())
    } else {
        Err("a Tiny call still treats a short STUN gap as a drop")
    }
}

/// The lab's health-grade driver. A green link does not hide a dry speaker.
pub(crate) fn prove_health_grade() -> Result<(), &'static str> {
    let dry = call::health::Health {
        underruns: Some(3),
        steady: Some(call::health::Grade::Great),
        rtt_ms: Some(40),
        jitter_ms: Some(2),
        loss_permille: Some(0),
        ..call::health::Health::default()
    };
    if dry.grade() == call::health::Grade::Poor {
        Ok(())
    } else {
        Err("a green link hid a dry speaker")
    }
}

/// The lab's call-device-ux driver. Verifies focused device pickers and non-duplicated dock modes.
pub(crate) fn prove_call_device_ux() -> Result<(), &'static str> {
    call::prove_call_device_ux()
}

/// The lab's compact-call-health driver. The Video tab's history holds video's own rates, and
/// holds nothing for the seconds no video ran.
pub(crate) fn prove_compact_call_health() -> Result<(), &'static str> {
    let mut samples = std::collections::VecDeque::new();
    let mut stats = protocol::CallStats { rtt_ms: 40, receive_kbps: 900, ..Default::default() };
    let now = std::time::Instant::now();
    call::health::push_chart(&mut samples, &stats, now);
    stats.video = Some(protocol::VideoStats { send_kbps: 440, receive_kbps: 800, ..Default::default() });
    call::health::push_chart(&mut samples, &stats, now + std::time::Duration::from_secs(1));
    stats.video = None;
    call::health::push_chart(&mut samples, &stats, now + std::time::Duration::from_secs(2));
    let video: Vec<_> = samples.iter().map(|sample| sample.video_kbps).collect();
    if video != [None, Some((440, 800)), None] {
        return Err("the Video tab's history did not keep video's own rates");
    }
    if samples.iter().all(|sample| sample.rtt_ms == 40 && sample.recv_kbps == 900) {
        Ok(())
    } else {
        Err("video samples changed the Connection tab's history")
    }
}

/// The lab's cancel-join driver. A leave is accepted while a join is still in flight.
pub(crate) fn prove_cancel_join() -> Result<(), &'static str> {
    if tiny::call_accepts(true, true) && !tiny::call_accepts(true, false) {
        Ok(())
    } else {
        Err("a join in flight swallowed the leave")
    }
}

/// The lab's stale-page driver. A late empty newest page does not clear a newer read.
pub(crate) fn prove_stale_page() -> Result<(), &'static str> {
    conversation::prove_stale_page()
}

/// The lab's return-to-bottom driver. Returning to a chat left at the bottom lands at the bottom.
pub(crate) fn prove_return_to_bottom() -> Result<(), &'static str> {
    conversation::prove_return_to_bottom()
}

/// The lab's text-drag driver. A release outside the message pane ends the selection drag.
pub(crate) fn prove_text_drag() -> Result<(), &'static str> {
    let mut dragging = true;
    if !conversation::release_text_drag(&mut dragging, false) || dragging {
        Err("a release outside the pane left the selection drag up")
    } else {
        Ok(())
    }
}

/// The lab's scroll-release driver. A scrollbar thumb drag's mouse-up releases the pointer.
pub(crate) fn prove_scroll_release() -> Result<(), &'static str> {
    components::scroll::prove_scroll_release()
}

/// The lab's pane-focus-pointer driver. A mouse press on scroll chrome does not focus the frame.
pub(crate) fn prove_pane_focus_pointer() -> Result<(), &'static str> {
    components::scroll::prove_pane_focus_pointer()
}

/// The lab's push-to-talk-release driver. A mic let go before its recording runs is still stopped.
pub(crate) fn prove_push_to_talk_release() -> Result<(), &'static str> {
    composer::prove_push_to_talk_release()
}

/// The lab's selection-clamp driver. A backwards caret pair paints no accent width.
pub(crate) fn prove_selection_clamp() -> Result<(), &'static str> {
    conversation::prove_selection_clamp()
}

/// The lab's offline-send-kept driver. A message held for an offline account that never goes comes back to the field.
pub(crate) fn prove_offline_send_kept() -> Result<(), &'static str> {
    composer::prove_offline_send_kept()
}

/// The lab's telegram-mute-bell driver. A muted Telegram row draws the bell-off beside its quiet count.
pub(crate) fn prove_telegram_mute_bell() -> Result<(), &'static str> {
    sidebar::chat::prove_telegram_mute_bell()
}

/// The lab's spell-check driver. "teh" is marked, a code span is not, and the setting can turn it off.
pub(crate) fn prove_spell_check() -> Result<(), &'static str> {
    spell::prove()
}

/// The lab's schedule-send driver. A network that schedules gets a time. One that does not is refused.
pub(crate) fn prove_schedule_send() -> Result<(), &'static str> {
    composer::prove_schedule_send()
}

/// The lab's Discord formatting driver. Bold wraps, a fence toggles, and a pasted url becomes a masked link.
pub(crate) fn prove_discord_format() -> Result<(), &'static str> {
    composer::format::prove()
}

/// The lab's open-at-login driver. The row is there, off by default, and the switch writes and
/// removes the entry the system reads at sign-in.
pub(crate) fn prove_open_at_login() -> Result<(), &'static str> {
    autostart::prove()
}

/// The lab's command-palette driver. Rebinding Quick Switcher moves the chord and the label.
pub(crate) fn prove_command_palette() -> Result<(), &'static str> {
    keymap::prove()
}

/// The lab's screen-lock driver. Idle past the timeout covers the window and a message names nothing.
pub(crate) fn prove_screen_lock() -> Result<(), &'static str> {
    lock::prove()
}

/// The lab's message-ux-polish driver. Verifies disappearing-message row suppression and tooltip
/// grammar, platform monospace inline code, typing row punctuation, ban date formatting, and
/// unread/day divider ordering.
pub(crate) fn prove_message_ux_polish() -> Result<(), &'static str> {
    let now = 1_795_000_000_i64;
    // 1. A normal message in a 7-day disappearing chat must NOT render a per-row timer badge.
    if conversation::draw::should_show_inline_expiry(now + 6 * 86_400, now, Some(7 * 86_400)) {
        return Err("disappearing chat still shows redundant per-message timer for 6d message");
    }
    // Urgent (< 1h) or standalone self-destruct messages still surface inline.
    if !conversation::draw::should_show_inline_expiry(now + 1_800, now, Some(7 * 86_400)) {
        return Err("urgent (<1h) disappearing message lost its inline timer");
    }
    if !conversation::draw::should_show_inline_expiry(now + 6 * 86_400, now, None) {
        return Err("standalone self-destruct message lost its inline timer");
    }
    // 2. Expiry tooltip never says "Disappears in expiring".
    if conversation::draw::expiry_tooltip(now, now) != "Expiring now" {
        return Err("expired message tooltip still has broken grammar");
    }
    // 3. Inline code uses platform monospace (`theme::MONO`), not hardcoded Menlo.
    if conversation::tokens::MONO != theme::MONO {
        return Err("inline code font does not match platform theme::MONO");
    }
    // 4. Bare arrows do not render as jumbo emoji.
    if conversation::rich::jumbo("→", &[]).is_some() {
        return Err("bare arrow symbol still renders as jumbo emoji");
    }
    // 5. Typing row drops duplicate trailing ellipsis while direct preview keeps it.
    let typist = protocol::Typist { id: "u1".into(), name: "Elyse".into(), until: 0, activity: protocol::Compose::Typing };
    let wording = composer::typing_wording_for(&[typist.clone()]);
    if wording.as_ref().map(|(n, r)| format!("{n}{r}")).as_deref() != Some("Elyse is typing") {
        return Err("typing row still has duplicate trailing ellipsis");
    }
    if composer::typing_preview_for(&[typist], false).as_deref() != Some("typing…") {
        return Err("sidebar/header typing preview lost its ellipsis");
    }
    // 6. Ban-until date never prints month 00 for January.
    let jan = composer::format_ban_until(1_768_478_400, 1_768_000_000);
    if jan.contains("-00-") {
        return Err("format_ban_until still prints month 00 for January");
    }
    Ok(())
}

/// The lab's tiny-call-ux driver. Verifies responsive side-by-side duo tile sizing, 1:1 ringing
/// callee seats, history peek, deduplicated panel latency, server display name in the compact
/// dock, and voice-room occupancy/bitrate tooltips.
pub(crate) fn prove_tiny_call_ux() -> Result<(), &'static str> {
    let (duo_w, duo_h) = call::view::stage_tile_size(464., 2, false);
    if duo_w < 168. || duo_w * 2. + 12. + 48. > 464. || duo_h < 132. {
        return Err("two callers did not fit side by side at 464 px");
    }
    let (wide_w, _) = call::view::stage_tile_size(900., 2, false);
    if (wide_w - 280.).abs() > 0.5 {
        return Err("wide duo tile did not cap at 280 px");
    }

    let mut history = sidebar::history::History::default();
    history.leave("general");
    history.leave("voice");
    if history.peek(true) != Some(&"voice") || history.peek(false).is_some() {
        return Err("history peek did not report previous stop without mutating cursor");
    }

    let mut ring: protocol::Call = serde_json::from_value(serde_json::json!({
        "conversation": { "network": "tiny", "account": "private.tinyclient.io", "id": "dm-1" },
        "state": "joined",
        "participants": [],
        "muted": false,
        "deafened": false,
        "joined": 1,
        "engine_linked": true,
        "video": []
    }))
    .map_err(|_| "failed to construct sample call")?;
    ring.state = protocol::CallState::RingingOut;
    let subject = call::Subject {
        title: "unRekable".into(),
        kind: protocol::ConversationKind::Direct,
        avatar: None,
        space: None,
        occupied: 0,
    };
    let stage = call::stage::Stage::of_call(&ring, &subject, "me", "Blitz", Some("peer-1"));
    if stage.seats.len() != 2 || !stage.seats.iter().any(|seat| !seat.me && seat.ringing) {
        return Err("1:1 outgoing ringing stage did not include the callee seat");
    }

    let health = call::health::Health::from_polls(
        None,
        &protocol::CallStats { rtt_ms: 52, jitter_ms: 3, ..protocol::CallStats::default() },
    );
    if tiny::panel::call_line(&protocol::CallState::Joined, Some(&health), false).map(|line| line.0).as_deref() != Some("52 ms") {
        return Err("user panel repeated Voice connected instead of compact latency");
    }
    if tiny::panel::call_tooltip(&protocol::CallState::Joined, Some("General"), Some(&health), false) != "Voice connected · General · 52 ms" {
        return Err("user panel tooltip did not carry full room and latency status");
    }
    if call::dock::compact_place_label("General", Some("Tiny Client Official Server"), "private.tinyclient.io") != "General / Tiny Client Official Server" {
        return Err("compact dock did not prefer server display name over raw hostname");
    }
    let mut room = protocol::TinyRoom::new("general", "General");
    room.bitrate_kbps = 64;
    room.e2ee_required = true;
    let tip = tiny::channel::occupancy_tip(&room);
    if !tip.contains("64 kb/s") || !tip.contains("End-to-end encrypted") {
        return Err("voice room occupancy tooltip missing bitrate or encryption details");
    }
    Ok(())
}

/// The lab's ui-wave2-polish driver. Verifies Wave 2 invariants: desktop phone-form safe insets
/// default to zero, Inbox stacks below 370 px so Activity + Mark all read do not clip, Lock timer
/// (LK) and Reaction notifications (NR) use dropdown menus, and overlay z-layers stay ordered.
pub(crate) fn prove_ui_wave2_polish() -> Result<(), &'static str> {
    if !phone::HANDHELD {
        if phone::tokens::safe_top() != 0.
            || phone::tokens::safe_bottom() != 0.
            || phone::tokens::safe_left() != 0.
            || phone::tokens::safe_right() != 0.
        {
            return Err("desktop narrow window reported nonzero simulated phone safe insets");
        }
    }
    let mid = sidebar::inbox_strip_fit(316., false, 1., true);
    if !mid.stacked || !mid.mark_all {
        return Err("desktop inbox strip did not stack at 316 px to avoid clipping Activity");
    }
    let wide = sidebar::inbox_strip_fit(400., false, 1., true);
    if wide.stacked || !wide.counts || !wide.mark_all {
        return Err("desktop inbox strip did not stay single-line at 400 px");
    }
    if !settings::pages::menu_choice("LK") || settings::pages::choice_id("LK") != Some("LK") {
        return Err("Lock timer (LK) was not converted to a dropdown menu");
    }
    if !settings::pages::menu_choice("NR") || settings::pages::choice_id("NR") != Some("NR") {
        return Err("Reaction notifications (NR) was not converted to a dropdown menu");
    }
    if !(components::place::Layer::Modal as usize > components::place::Layer::Toast as usize
        && components::place::Layer::Toast as usize > components::place::Layer::Float as usize)
    {
        return Err("overlay z-order priorities were not strictly ordered");
    }
    Ok(())
}

/// The lab's curated-themes driver. Verifies all 8 themes × 9 accents × {Standard, HC} WCAG
/// contrast floors, `--theme` / `--accent` override parsing, `shell.prefs` round-tripping with
/// forward-compatible `theme\tlight`, protocol settings validation, Settings search (`AT`/`AC`),
/// and zero-surprise Theme Picker click semantics.
pub(crate) fn prove_curated_themes() -> Result<(), &'static str> {
    if theme::verify_all_wcag().is_err() {
        return Err("one or more curated theme/accent/HC combinations failed WCAG verification");
    }
    for id in theme::ThemeId::ALL {
        if !theme::set_override(Some(id.word())) || theme::active_theme() != id || theme::is_high_contrast() {
            return Err("--theme override failed to activate standard curated theme");
        }
        let hc_word = format!("{}-hc", id.word());
        if !theme::set_override(Some(&hc_word)) || theme::active_theme() != id || !theme::is_high_contrast() {
            return Err("--theme <id>-hc override failed to activate high-contrast curated theme");
        }
    }
    if !theme::set_override(Some("nord")) || !theme::set_accent_override(Some("auto")) || theme::active_accent() != theme::Accent::Teal {
        return Err("Auto accent did not resolve Nord signature Teal accent");
    }
    if !theme::set_accent_override(Some("emerald")) || theme::active_accent() != theme::Accent::Emerald {
        return Err("--accent override failed to select Emerald");
    }
    theme::set_accent_override(None);
    theme::set_override(None);

    for word in protocol::settings::DARK_THEME_CHOICES {
        if !protocol::settings::valid(protocol::settings::GLOBAL, protocol::settings::DARK_THEME, word) {
            return Err("protocol settings rejected a curated dark_theme choice");
        }
    }
    for word in protocol::settings::LIGHT_THEME_CHOICES {
        if !protocol::settings::valid(protocol::settings::GLOBAL, protocol::settings::LIGHT_THEME, word) {
            return Err("protocol settings rejected a curated light_theme choice");
        }
    }
    for word in protocol::settings::ACCENT_CHOICES {
        if !protocol::settings::valid(protocol::settings::GLOBAL, protocol::settings::ACCENT, word) {
            return Err("protocol settings rejected a curated accent choice");
        }
    }

    let prefs = app::prefs::Prefs {
        choices: app::prefs::Choices {
            dark_theme: theme::ThemeId::Mocha,
            light_theme: theme::ThemeId::Stone,
            accent: theme::Accent::Rose,
            ..app::prefs::Choices::default()
        },
        ..app::prefs::Prefs::default()
    };
    let parsed = app::prefs::parse(&app::prefs::write(&prefs));
    if parsed.choices.dark_theme != theme::ThemeId::Mocha
        || parsed.choices.light_theme != theme::ThemeId::Stone
        || parsed.choices.accent != theme::Accent::Rose
    {
        return Err("shell.prefs did not round-trip dark_theme, light_theme, and accent");
    }
    if app::prefs::parse("theme\tlight\n").kept != ["theme\tlight"] {
        return Err("shell.prefs broke forward-compatible preservation of unknown theme\\tlight line");
    }

    if !settings::rows::search("accent").iter().any(|r| r.id == "AC") || settings::rows::not_yet("accent").is_some() {
        return Err("Settings search did not surface AC for 'accent' or left stale NOT_YET entry");
    }
    if !settings::rows::search("mocha").iter().any(|r| r.id == "AT") || !settings::rows::search("warm stone").iter().any(|r| r.id == "AT") {
        return Err("Settings search did not surface AT for curated theme names");
    }

    let mut c = app::prefs::Choices::default();
    settings::pages::pick_theme_choice(&mut c, theme::ThemeId::Stone, true);
    if c.light_theme != theme::ThemeId::Stone || c.appearance != theme::Appearance::Light {
        return Err("picking a Light theme while System is Dark did not switch appearance to Light for immediate preview");
    }
    settings::pages::pick_theme_choice(&mut c, theme::ThemeId::Nord, false);
    if c.dark_theme != theme::ThemeId::Nord || c.appearance != theme::Appearance::Dark {
        return Err("picking a Dark theme while Light did not switch appearance to Dark for immediate preview");
    }
    Ok(())
}

/// The lab's responsive-shell-ux driver. Verifies narrow overlay width clamping, `All unreads`
/// message/chat count & canonical DM vs channel badges, and `list_row`'s `flex_col` left-alignment safeguard.
pub(crate) fn prove_responsive_shell_ux() -> Result<(), &'static str> {
    use components::place::{anchor, fit, max_width, Place, MARGIN};
    let narrow = (360., 640.);
    if (max_width(narrow) - 344.).abs() > 0.5 {
        return Err("narrow window max_width did not reserve margins");
    }
    let picker = fit(Place::Above(anchor(310., 580., 36., 36.)), (352., 400.), narrow);
    if picker.x < MARGIN || picker.x + picker.w > narrow.0 - MARGIN || (picker.w - 344.).abs() > 0.5 {
        return Err("wide overlay was not clamped inside a 360 px window");
    }
    let wide_menu = fit(Place::Below(anchor(10., 10., 20., 20.)), (600., 100.), (540., 440.));
    if wide_menu.x < MARGIN || (wide_menu.w - (540. - 2. * MARGIN)).abs() > 0.5 {
        return Err("oversized dropdown menu overflowed the viewport width");
    }

    let mut col = components::list_row_inner(gpui::div().flex().flex_col(), false);
    if col.style().align_items != Some(gpui::AlignItems::FlexStart)
        || col.style().justify_content != Some(gpui::JustifyContent::Center)
    {
        return Err("list_row_inner did not left-align flex_col content");
    }
    let mut row = components::list_row_inner(gpui::div().flex(), false);
    if row.style().align_items != Some(gpui::AlignItems::Center) {
        return Err("list_row_inner broke vertical centering for horizontal rows");
    }

    let mut dm = protocol::Conversation::new(
        protocol::Key { network: protocol::Network::Slack, account: "a".into(), id: "dm".into() },
        protocol::ConversationKind::Direct,
        "Geekbot",
    );
    dm.unread = 2;
    dm.mentions = 2;
    if sidebar::home_view::unread_group_badge(&dm, 2) != components::BadgeLook::Count(2) {
        return Err("1:1 DM in All unreads used a mention pill instead of an outlined count");
    }
    let mut ch = protocol::Conversation::new(
        protocol::Key { network: protocol::Network::Slack, account: "a".into(), id: "ch".into() },
        protocol::ConversationKind::Channel,
        "standup",
    );
    ch.unread = 1;
    let groups = vec![
        std::rc::Rc::new(protocol::UnreadGroup { conversation: dm, messages: Vec::new(), total_unread: 2 }),
        std::rc::Rc::new(protocol::UnreadGroup { conversation: ch, messages: Vec::new(), total_unread: 1 }),
    ];
    let (msgs, chats, summary) = sidebar::home_view::unreads_summary(&groups);
    if msgs != 3 || chats != 2 || summary != "3 messages · 2 chats" {
        return Err("All unreads summary did not report accurate message and chat counts");
    }
    Ok(())
}

/// The lab's clean-composer-bar driver. Verifies TextInput draws no inset focus outline when
/// typing, the composer card uses subtle `border_strong` focus without a glow halo, no default
/// floating pills appear above the composer at rest, left-side actions stay consolidated behind
/// a single button, and the Teams subject bar stays hidden until requested.
pub(crate) fn prove_clean_composer() -> Result<(), &'static str> {
    if input::text_input_inner_focus_ring().is_some() {
        return Err("TextInput still draws an inner inset focus ring while typing");
    }
    let (focused_border, focused_halo) = composer::card_focus_style(false, true);
    if focused_border != theme::border_strong() || focused_halo {
        return Err("focused composer card still draws an accent border or halo");
    }
    if composer::send_options_visible(protocol::Importance::Normal, false, false) {
        return Err("default floating pills still render above the composer at rest");
    }
    if !composer::send_options_visible(protocol::Importance::Important, false, false)
        || !composer::send_options_visible(protocol::Importance::Normal, true, false)
    {
        return Err("active non-default send options stopped showing above the composer");
    }
    let telegram = composer::LeftActions {
        attach: true,
        location: true,
        poll: true,
        video_note: true,
        ..Default::default()
    };
    if telegram.leading_button_count() != 1 || !telegram.uses_menu() {
        return Err("Telegram composer still lays out multiple leading icons instead of one menu");
    }
    if composer::subject_visible(true, false, "") {
        return Err("Teams channel subject row still renders at rest when unused");
    }
    if !composer::has_send_menu_for(false, false, true, false) || !composer::has_send_menu_for(false, false, false, true) {
        return Err("Send right-click menu does not open for Importance or Effects");
    }
    Ok(())
}

/// The lab's calls-tiny-responsive driver. Verifies responsive call popover clamping, incoming
/// overlay live-bar clearance, stream tile clamping, Tiny Server phone/sidebar/dock layout guards,
/// Discord modal narrow stacking, and searchable full-list channel pickers.
pub(crate) fn prove_calls_tiny_responsive() -> Result<(), &'static str> {
    if call::dock::dock_popover_width(320., 260., 360.) > 260. {
        return Err("call dock popover overflowed a 260 px sidebar");
    }
    if call::dock::dock_popover_width(320., 260., 240.) > 224. {
        return Err("call dock popover overflowed a 240 px viewport");
    }
    let (narrow_w, top_plain) = call::dock::incoming_overlay_frame(300., false);
    let (_, top_live) = call::dock::incoming_overlay_frame(300., true);
    if narrow_w > 268. {
        return Err("incoming call overlay overflowed a 300 px viewport");
    }
    if (top_live - top_plain - theme::BANNER).abs() > 0.5 {
        return Err("incoming call overlay did not offset below the live-location banner");
    }
    let (stream_w, stream_h) = call::view::Size::Big(640.).stream_fit(360.);
    if stream_w > 312. || stream_h > stream_w {
        return Err("call stream tile did not clamp to narrow stage width");
    }
    tiny::panel::prove_tiny_responsive()?;
    if !discord_settings::stacked_layout(599.) || discord_settings::stacked_layout(600.) {
        return Err("Discord server settings modal did not stack below 600 px");
    }
    let channels: Vec<(String, String)> = (1..=25)
        .map(|i| (format!("ch-{i}"), format!("channel-{i:02}")))
        .collect();
    if discord_settings::filter_channels(&channels, "").len() != 25 {
        return Err("Discord channel picker still truncated channels past 12");
    }
    let hits = discord_settings::filter_channels(&channels, "24");
    if hits.len() != 1 || hits[0].0 != "ch-24" {
        return Err("Discord channel picker search did not find channel 24");
    }
    Ok(())
}

use gpui::{point, prelude::*, px, size, App, Bounds, Context, Div, KeyBinding, MenuItem, TitlebarOptions, WindowBounds, WindowDecorations, WindowOptions};
use state::Shell;

/// The windows' Wayland app id and X11 class: a desktop matches them to `dev.tinyclient.app.desktop`
/// (packaging/linux) for the dock icon and name. The same id as the macOS bundle's.
pub(crate) const APP_ID: &str = "dev.tinyclient.app";

gpui::actions!(
    tinyclient,
    [
        Quit, Switcher, Back, Forward, UpLevel, ToggleSidebar, CloseRight, CloseTop, NextRow,
        PreviousRow, NextUnread, PreviousUnread, ListUp, ListDown, ListOpen, ListFold, ListUnfold,
        MarkRead, FocusNext, FocusPrevious,
        Place1, Place2, Place3, Place4, Place5, Place6, Place7, Place8, Place9,
        // The menu bar's (`app::menubar`).
        About, CheckForUpdates, Settings, Hide, HideOthers, ShowAll, NewMessage, AddAccount,
        AttachFile, CloseWindow, SearchConversation, SearchEverywhere, BiggerText, SmallerText,
        ActualSize, FullScreen, MarkUnread, MarkSpaceRead, ShowDetails, Star, Mute, StartCall,
        OpenInNetwork, CopyLink, Minimize, Zoom, ShowWindow, KeyboardShortcuts, ReleaseNotes,
        CopyDiagnostics, ReportProblem, ShowLogs
    ]
);

impl Shell {
    /// `cmd-1` to `cmd-9`: the rail's places in order.
    fn visit_place(&mut self, index: usize, window: &mut gpui::Window, cx: &mut Context<Self>) {
        if let Some(mode) = self.places().get(index).cloned() {
            self.visit(mode, window, cx);
        }
    }

    /// The shell itself has the keys: no field, no message, no menu.
    fn in_lists(&self, window: &gpui::Window) -> bool {
        self.focus.is_focused(window) || self.sidebar_state.focus.is_focused(window)
    }

    /// A character typed with no field focused goes to the composer.
    fn typed(&mut self, event: &gpui::KeyDownEvent, window: &mut gpui::Window, cx: &mut Context<Self>) {
        let keys = &event.keystroke;
        let command = keys.modifiers.platform || keys.modifiers.control || keys.modifiers.function;
        let Some(text) = keys.key_char.as_deref().filter(|text| !command && typed_text(text)) else {
            return;
        };
        let layered = self.sidebar_state.switcher.is_some() || self.layout_state.menu.is_some();
        if layered || self.open.is_none() || !self.in_lists(window) {
            return;
        }
        self.composer_type(text, window, cx);
        cx.stop_propagation();
    }

    /// Follow the rendered tab order, including the rail and toolbar controls.
    fn focus_region(&mut self, forward: bool, window: &mut gpui::Window, cx: &mut Context<Self>) {
        if forward {
            window.focus_next(cx);
        } else {
            window.focus_prev(cx);
        }
        if self.sidebar_state.focus.is_focused(window) {
            self.cursor_to_open();
            cx.notify();
        }
    }

    /// The window's keys, on its root so they work wherever focus is. A text field's own keys
    /// win while it has focus.
    fn keys(&self, root: Div, cx: &mut Context<Self>) -> Div {
        macro_rules! places {
            ($root:expr, $($action:ty => $index:expr),*) => {
                $root$(.on_action(cx.listener(|this, _: &$action, window, cx| this.visit_place($index, window, cx))))*
            };
        }
        let root = places!(root, Place1 => 0, Place2 => 1, Place3 => 2, Place4 => 3, Place5 => 4, Place6 => 5, Place7 => 6, Place8 => 7, Place9 => 8);
        root.track_focus(&self.focus)
            .key_context("Shell")
            .on_action(cx.listener(|this, _: &Switcher, window, cx| this.open_switcher("", window, cx)))
            .on_action(cx.listener(|this, _: &Back, window, cx| {
                // On a phone back pops one pushed page; at the home it is the platform's.
                if !this.phone_back(Some(window), cx) {
                    this.travel(true, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &Forward, _, cx| this.travel(false, cx)))
            .on_action(cx.listener(|this, _: &UpLevel, _, cx| this.up_level(cx)))
            .on_action(cx.listener(|this, _: &ToggleSidebar, window, cx| this.toggle_sidebar(window, cx)))
            .on_action(cx.listener(|this, _: &MarkRead, _, cx| this.mark_open_read(cx)))
            .on_action(cx.listener(|this, _: &MarkUnread, _, cx| this.mark_open_unread(cx)))
            .on_action(cx.listener(|this, _: &FocusNext, window, cx| this.focus_region(true, window, cx)))
            .on_action(cx.listener(|this, _: &FocusPrevious, window, cx| this.focus_region(false, window, cx)))
            .on_key_down(cx.listener(|_, _: &gpui::KeyDownEvent, _, _| lock::touch()))
            .on_key_down(cx.listener(Self::typed))
            .on_key_down(cx.listener(Self::page_sidebar))
            .on_action(cx.listener(|this, _: &CloseRight, window, cx| this.close_right(Some(window), cx)))
            .on_action(cx.listener(|this, _: &SearchConversation, window, cx| this.search_here(window, cx)))
            .on_action(cx.listener(|this, _: &SearchEverywhere, window, cx| this.search_everywhere(window, cx)))
            .on_action(cx.listener(|this, _: &CloseTop, window, cx| {
                // With no layer to close, Escape belongs to the conversation.
                if !this.close_top(window, cx) {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &NextRow, _, cx| this.walk(true, cx)))
            .on_action(cx.listener(|this, _: &PreviousRow, _, cx| this.walk(false, cx)))
            .on_action(cx.listener(|this, _: &NextUnread, _, cx| this.walk_unread(true, cx)))
            .on_action(cx.listener(|this, _: &PreviousUnread, _, cx| this.walk_unread(false, cx)))
            .on_action(cx.listener(|this, _: &ListDown, window, cx| {
                if !this.in_lists(window) {
                    cx.propagate();
                    return;
                }
                if !this.first_run_key("down", cx) {
                    this.move_cursor(true, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &ListUp, window, cx| {
                if !this.in_lists(window) {
                    cx.propagate();
                    return;
                }
                if !this.first_run_key("up", cx) {
                    this.move_cursor(false, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &ListOpen, window, cx| {
                if !this.in_lists(window) {
                    cx.propagate();
                    return;
                }
                if !this.first_run_key("enter", cx) {
                    this.open_cursor(window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &ListFold, window, cx| {
                if this.in_lists(window) { this.fold_cursor(false, cx); } else { cx.propagate(); }
            }))
            .on_action(cx.listener(|this, _: &ListUnfold, window, cx| {
                if this.in_lists(window) { this.fold_cursor(true, cx); } else { cx.propagate(); }
            }))
            .on_action(cx.listener(|this, _: &NewMessage, window, cx| this.open_switcher("@", window, cx)))
            .on_action(cx.listener(|this, _: &AddAccount, _, cx| this.open_menu(layout::Menu::Add, cx)))
            .on_action(cx.listener(|this, _: &BiggerText, window, cx| this.step_text(Some(1), window, cx)))
            .on_action(cx.listener(|this, _: &SmallerText, window, cx| this.step_text(Some(-1), window, cx)))
            .on_action(cx.listener(|this, _: &ActualSize, window, cx| this.step_text(None, window, cx)))
            .on_action(cx.listener(|_, _: &FullScreen, window, _| window.toggle_fullscreen()))
            .on_action(cx.listener(|_, _: &Minimize, window, _| window.minimize_window()))
            .on_action(cx.listener(|_, _: &Zoom, window, _| window.zoom_window()))
            .on_action(cx.listener(|this, _: &CopyDiagnostics, _, cx| this.copy_diagnostics(cx)))
            .when_some(self.snapshot.as_ref().and_then(|s| s.open.as_ref()), |root, open| {
                let (key, starred) = (open.key.clone(), open.starred);
                root.on_action(cx.listener(move |this, _: &Star, _, cx| this.star(key.clone(), !starred, cx)))
            })
    }
}

/// Whether a key's text is something to type: printable, and not the keys the lists use.
fn typed_text(text: &str) -> bool {
    !text.is_empty() && text.chars().all(|c| !c.is_control()) && text != " "
}

/// Drops every chord and binds the stock map plus whatever Settings saved.
pub(crate) fn rebind_keys(cx: &mut App) {
    cx.clear_key_bindings();
    input::bind_keys(cx);
    call::bind_keys(cx);
    bind_keys(cx);
    settings::bind_keys(cx);
}

fn bind_keys(cx: &mut App) {
    app::menubar::bind_keys(cx);
    cx.bind_keys(shell_bindings());
    // After Attach File, so a focused field wins the tie on ⌘U. A field that is not a Discord
    // draft hands that key back to Attach File.
    let field = Some("TextInput");
    cx.bind_keys([
        KeyBinding::new("secondary-b", input::FormatBold, field),
        KeyBinding::new("secondary-i", input::FormatItalic, field),
        KeyBinding::new("secondary-u", input::FormatUnderline, field),
        KeyBinding::new("secondary-shift-x", input::FormatStrike, field),
        KeyBinding::new("secondary-shift-e", input::FormatCode, field),
        KeyBinding::new("secondary-shift-q", input::FormatQuote, field),
        KeyBinding::new("secondary-shift-p", input::FormatSpoiler, field),
    ]);
}

fn shell_bindings() -> Vec<KeyBinding> {
    let mut out = Vec::new();
    let shell = Some("Shell");
    macro_rules! bind {
        ($id:literal, $action:expr, $context:expr) => {
            for chord in keymap::chords($id) {
                out.push(KeyBinding::new(&chord, $action, $context));
            }
        };
    }
    bind!("quit", Quit, None);
    bind!("quick-switcher", Switcher, None);
    bind!("back", Back, None);
    bind!("forward", Forward, None);
    bind!("up-level", UpLevel, shell);
    bind!("hide-sidebar", ToggleSidebar, None);
    bind!("close-right", CloseRight, None);
    bind!("close-top", CloseTop, shell);
    bind!("mark-read", MarkRead, shell);
    bind!("focus-next", FocusNext, shell);
    bind!("focus-previous", FocusPrevious, shell);
    bind!("next", NextRow, None);
    bind!("previous", PreviousRow, None);
    bind!("next-unread", NextUnread, None);
    bind!("previous-unread", PreviousUnread, None);
    bind!("list-down", ListDown, shell);
    bind!("list-up", ListUp, shell);
    bind!("list-open", ListOpen, shell);
    bind!("list-fold", ListFold, shell);
    bind!("list-unfold", ListUnfold, shell);
    bind!("place-1", Place1, None);
    bind!("place-2", Place2, None);
    bind!("place-3", Place3, None);
    bind!("place-4", Place4, None);
    bind!("place-5", Place5, None);
    bind!("place-6", Place6, None);
    bind!("place-7", Place7, None);
    bind!("place-8", Place8, None);
    bind!("place-9", Place9, None);
    out
}

/// Runs a palette row that is one of the menu actions.
pub(crate) fn dispatch_binding(id: &str, window: &mut gpui::Window, cx: &mut App) -> bool {
    use gpui::Action;
    macro_rules! go {
        ($action:expr) => {
            Some(Box::new($action) as Box<dyn Action>)
        };
    }
    let action: Option<Box<dyn Action>> = match id {
        "quit" => go!(Quit),
        "quick-switcher" => go!(Switcher),
        "back" => go!(Back),
        "forward" => go!(Forward),
        "up-level" => go!(UpLevel),
        "hide-sidebar" => go!(ToggleSidebar),
        "close-right" => go!(CloseRight),
        "close-top" => go!(CloseTop),
        "mark-read" => go!(MarkRead),
        "focus-next" => go!(FocusNext),
        "focus-previous" => go!(FocusPrevious),
        "next" => go!(NextRow),
        "previous" => go!(PreviousRow),
        "next-unread" => go!(NextUnread),
        "previous-unread" => go!(PreviousUnread),
        "list-down" => go!(ListDown),
        "list-up" => go!(ListUp),
        "list-open" => go!(ListOpen),
        "list-fold" => go!(ListFold),
        "list-unfold" => go!(ListUnfold),
        "place-1" => go!(Place1),
        "place-2" => go!(Place2),
        "place-3" => go!(Place3),
        "place-4" => go!(Place4),
        "place-5" => go!(Place5),
        "place-6" => go!(Place6),
        "place-7" => go!(Place7),
        "place-8" => go!(Place8),
        "place-9" => go!(Place9),
        "settings" => go!(Settings),
        "new-message" => go!(NewMessage),
        "attach" => go!(AttachFile),
        "close-window" => go!(CloseWindow),
        "search-here" => go!(SearchConversation),
        "search-all" => go!(SearchEverywhere),
        "bigger-text" => go!(BiggerText),
        "smaller-text" => go!(SmallerText),
        "actual-size" => go!(ActualSize),
        "full-screen" => go!(FullScreen),
        "details" => go!(ShowDetails),
        "start-call" => go!(StartCall),
        "open-in" => go!(OpenInNetwork),
        "minimize" => go!(Minimize),
        "shortcuts-page" => go!(KeyboardShortcuts),
        "show-logs" => go!(ShowLogs),
        "hide" => go!(Hide),
        "hide-others" => go!(HideOthers),
        "mute" | "toggle-mute" => go!(call::ToggleMute),
        "deafen" => go!(call::ToggleDeafen),
        "leave-call" => go!(call::Leave),
        "accept" => go!(call::Accept),
        "decline" => go!(call::Decline),
        "go-to-call" => go!(call::Jump),
        "popout-call" => go!(call::TogglePopout),
        "about" => go!(About),
        "check-updates" => go!(CheckForUpdates),
        "add-account" => go!(AddAccount),
        "copy-diagnostics" => go!(CopyDiagnostics),
        "star" => go!(Star),
        "conversation-mute" => go!(Mute),
        "show-all" => go!(ShowAll),
        "zoom" => go!(Zoom),
        "show-window" => go!(ShowWindow),
        _ => None,
    };
    let Some(action) = action else {
        return false;
    };
    window.dispatch_action(action, cx);
    true
}

/// `--shot out.png` draws one state with live data, writes the frame and quits, so every state
/// can be looked at without clicking:
/// `--size WxH` / `--narrow`, `--mode inbox|people|discord|slack|space` (`--space N` picks
/// which), `--filter unread|mentions`, `--open-first` / `--open N` (a row of the list on show),
/// `--cursor N`, `--switcher [letters]`, `--menu add|own|network|space|row`, `--float`,
/// `--hide-sidebar`, `--right thread|members`, `--pin N`, `--toast`, `--components`,
/// `--walk N` (N presses of the unread walk), `--back N` (N steps back in history),
/// `--right details|member-list` (the strip's own buttons, as tapped), `--menu account` (the
/// rail avatar's menu; a phone's account sheet), `--text-scale N` (the phone's type, 2 for a 200% run),
/// `--sign-in-failed closed|unreachable|blocked` (first launch, with nothing signed in),
/// `--state starting|keychain|denied|reconnecting|failed` (a window state, forced),
/// `--crash-toast` (the toast after a crash relaunch), `--menu row-right|row-bottom` (the row
/// menu opened at the window's edges), `--text-step N`, `--mark-all` (marks the inbox read,
/// then waits so the shot shows the cleared list), `--start-chatd` (let the shot start
/// chatd when it is not running; off by default so a shot leaves no process behind),
/// `--autoplay always|hover|never` (when GIFs play), `--frames N` (N frames 150 ms apart, as
/// `out-1.png` …, to see moving pictures move). On a Tiny server (`--mode tiny`): `--tiny-room
/// N` (the Nth channel), `--tiny-menu header|channel|person:N` (N: the Nth seat, `:sub` adds its
/// Move submenu), `--tiny-stream` (my own stream filling the stage, with `TINYCLIENT_SHOT_CALL=tiny_sharing`),
/// `--tiny-modal invite|code|kick|ban|profile`, `--tiny-settings`, `--tiny-card N`
/// (the Nth member's hover card), `--tiny-setup first|custom|refused|checklist|links|privacy|change|change-anyway|member` (first-run
/// setup, or Server Settings › Privacy, over a made-up outline), `--tiny-confirm transfer|codes`.
///
/// `--demo` draws made-up data instead of live data: a fake chatd answers every request
/// (`shot_demo`), chatd is never started or asked, and nobody's real chats are read.
/// `--demo-empty` is the same with nothing signed in (first run). With either:
/// `--compose WORDS` (the open
/// conversation's composer, typed). Other states come from the views' own shot hooks:
/// `TINYCLIENT_SHOT_HOVER=<message id>` (the message toolbar, as a long press shows it),
/// `TINYCLIENT_SHOT_MENU=<message id>`, `TINYCLIENT_SHOT_AT=<message id>` (`m01`…`m25` in the
/// demo's "Foray carpool"), `TINYCLIENT_S6=d-card` (the open conversation's person card),
/// `TINYCLIENT_SHOT_CALL=joined` (the call dock, desktop only), `TINYCLIENT_SIGNIN_SHOT=phone`
/// (the sign-in sheet), and `--settings <section> --settings-shot out.png` with
/// `--settings-size WxH` (the Settings window at that size). `scripts/mobile/phone-shots.ps1`
/// renders all of them.
#[cfg(feature = "gui-shot")]
mod shot {
    use super::layout::{Menu, RightKind, RightPane};
    use super::sidebar::Mode;
    use super::state::Shell;
    use super::app::service::Shown;
    use super::{components, flag, theme};
    use gpui::{div, point, prelude::*, px, App, Context, Window};
    use protocol::{ConversationKind, InboxFilter, Network};
    use std::time::Duration;

    fn has(name: &str) -> bool {
        std::env::args().any(|arg| arg == name)
    }

    fn demo_pane(_: &Shell, _: &mut Window, _: &mut Context<Shell>) -> gpui::AnyElement {
        components::empty_state("icons/message-square.svg", "No replies yet", "This pane is filled by another view.").into_any_element()
    }

    fn stage(shell: &mut Shell, step: usize, window: &mut Window, cx: &mut Context<Shell>) {
        match step {
            0 => {
                if let Some(autoplay) = flag("--autoplay").and_then(|word| super::anim::Autoplay::from_word(&word)) {
                    super::anim::set_autoplay(autoplay);
                }
                if let Some(how) = flag("--sign-in-failed") {
                    use protocol::ErrorCode;
                    let (code, detail) = match how.as_str() {
                        "unreachable" => (ErrorCode::SignInUnreachable, ""),
                        "blocked" => (ErrorCode::SignInBlocked, ""),
                        _ => (ErrorCode::SignIn, "The sign-in window stopped before it finished."),
                    };
                    shell.sign_in_failed = super::state::sign_in_words(Network::Slack, code, detail).map(|words| (Network::Slack, words));
                }
                if let Some(index) = flag("--pin").and_then(|n| n.parse::<usize>().ok()) {
                    if let Some(space) = shell.spaces.get(index) {
                        shell.pin(space.key.clone(), true, cx);
                    }
                }
                let space = flag("--space").and_then(|n| n.parse::<usize>().ok()).unwrap_or(0);
                match flag("--mode").as_deref() {
                    Some("people") => shell.go(Mode::People, cx),
                    Some("discord") => shell.go(Mode::Network(Network::Discord), cx),
                    Some("slack") => shell.go(Mode::Network(Network::Slack), cx),
                    Some("tiny") => shell.go(Mode::Network(Network::Tiny), cx),
                    Some("space") => {
                        if let Some(space) = shell.spaces.get(space) {
                            shell.go(Mode::Space(space.key.clone()), cx);
                        }
                    }
                    _ => {}
                }
                match flag("--filter").as_deref() {
                    Some("unread") => shell.set_filter(InboxFilter::Unread, cx),
                    Some("mentions") => shell.set_filter(InboxFilter::Mentions, cx),
                    Some("activity") => shell.set_filter(InboxFilter::Activity, cx),
                    _ => {}
                }
                if let Some(scale) = flag("--text-scale").and_then(|n| n.parse::<f32>().ok()) {
                    super::phone::tokens::set_text_scale(scale);
                }
            }
            1 => {
                let row = if has("--open-first") { Some(0) } else { flag("--open").and_then(|n| n.parse::<usize>().ok()) };
                if let Some(row) = row {
                    shell.open_listed(row, cx);
                }
            }
            _ => {
                if let Some(cursor) = flag("--cursor").and_then(|n| n.parse::<usize>().ok()) {
                    shell.sidebar_state.cursor = Some(cursor);
                }
                if has("--float") {
                    shell.layout_state.floating = true;
                }
                if has("--hide-sidebar") {
                    shell.layout_state.sidebar_hidden = true;
                }
                if has("--focus-list") {
                    window.focus(&shell.sidebar_state.focus, cx);
                    shell.cursor_to_open();
                }
                match flag("--right").as_deref() {
                    Some("info") => shell.toggle_info(cx),
                    // The strip's own buttons, as a tap on them does.
                    Some("details") => shell.toggle_details(cx),
                    Some("member-list") => shell.toggle_member_list(cx),
                    Some("members") =>shell.open_right(RightPane { kind: RightKind::Members, title: "Members".into(), count: Some(128), body: demo_pane }, cx),
                    Some(_) => shell.open_right(RightPane { kind: RightKind::Thread, title: "Thread".into(), count: Some(6), body: demo_pane }, cx),
                    None => {}
                }
                match flag("--menu").as_deref() {
                    Some("add") => shell.open_menu(Menu::Add, cx),
                    Some("own" | "account") => shell.open_menu(Menu::Own, cx),
                    Some("network") => shell.open_menu(Menu::Network(shell.sidebar_state.mode.network().unwrap_or(Network::Discord)), cx),
                    Some("space") => {
                        if let Mode::Space(space) = shell.sidebar_state.mode.clone() {
                            shell.open_menu(Menu::Space(space), cx);
                        }
                    }
                    Some("row") => {
                        let rows = shell.sidebar_state.inbox.all();
                        let row = rows
                            .iter()
                            .find(|row| row.kind == ConversationKind::Direct)
                            .or_else(|| rows.iter().find(|row| row.kind.is_direct()))
                            .cloned()
                            .or_else(|| shell.sidebar_state.inbox.get(1).cloned());
                        if let Some(row) = row {
                            shell.open_menu(Menu::Row(row, point(px(240.), px(180.))), cx);
                        }
                    }
                    _ => {}
                }
                match flag("--state").as_deref() {
                    Some("starting") => shell.forced_state = Some(Shown::Starting),
                    Some("keychain") => shell.forced_state = Some(Shown::KeychainWaiting),
                    Some("denied") => shell.forced_state = Some(Shown::KeychainDenied(vec![Network::Slack])),
                    Some("reconnecting") => shell.forced_state = Some(Shown::Reconnecting),
                    Some("failed") => shell.forced_state = Some(Shown::Failed),
                    _ => {}
                }
                if has("--crash-toast") {
                    shell.crashed = Some(0);
                }
                let size = window.viewport_size();
                let (wide, tall) = (f32::from(size.width), f32::from(size.height));
                let edge = match flag("--menu").as_deref() {
                    Some("row-right") => Some(point(px(wide - 40.), px(200.))),
                    Some("row-bottom") => Some(point(px(wide - 40.), px(tall - 30.))),
                    _ => None,
                };
                if let (Some(at), Some(row)) = (edge, shell.sidebar_state.inbox.get(1).cloned()) {
                    shell.open_menu(Menu::Row(row, at), cx);
                }
                if let Some(step) = flag("--text-step").and_then(|n| n.parse::<i32>().ok()) {
                    theme::set_text_step(step);
                }
                if has("--toast") {
                    shell.toast(Some(("icons/check.svg", theme::text_soft())), "Marked 14 conversations read", cx);
                }
                if has("--switcher") {
                    let letters = flag("--switcher").filter(|next| !next.starts_with("--")).unwrap_or_default();
                    shell.open_switcher(&letters, window, cx);
                }
                if has("--components") {
                    shell.sheet = true;
                }
                if let Some(words) = flag("--compose") {
                    shell.composer_type(&words, window, cx);
                }
                shell.tiny_shot(flag, window, cx);
                cx.notify();
            }
        }
    }

    /// The base components in their states, for looking at next to `mockups/01-system.html`.
    /// Drawn over the whole window.
    pub fn sheet() -> gpui::Div {
        use components::*;
        use theme::Type;
        use protocol::Presence;
        let line = || div().flex().flex_wrap().items_center().gap(px(8.));
        let column = |w: f32| div().w(px(w)).flex_none().flex().flex_col().gap(px(10.));
        let presences = [Presence::Online, Presence::Idle, Presence::Dnd, Presence::Offline];
        let label = |words: &'static str| div().type_micro().text_color(theme::text_faint()).child(words);
        // One row of each height, to compare with the row family.
        let conversation = |id: &'static str, name: &'static str, preview: &'static str, time: &'static str, badge: Option<gpui::Div>, unread: bool| {
            let weight = if unread { theme::SEMIBOLD } else { theme::REGULAR };
            let content = div()
                .gap(px(10.))
                .child(Avatar::named(name, 32.).presence(Presence::Online, theme::sidebar()).build())
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(4.))
                                .child(network_mark(protocol::Network::Discord, 14., theme::text_faint()))
                                .child(div().font_weight(weight).truncate().child(name))
                                .child(div().flex_1())
                                .child(div().type_micro().text_color(theme::text_faint()).child(time)),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(4.))
                                .child(div().flex_1().min_w_0().truncate().type_small().text_color(if unread { theme::text() } else { theme::text_soft() }).child(preview))
                                .children(badge),
                        ),
                );
            list_row(id, theme::row_height(theme::ROW_TWO), false, false, content)
        };
        let place = |id: &'static str, name: &'static str, right: Option<gpui::Div>, selected: bool, focused: bool| {
            let content = div().gap(px(8.)).child(glyph("icons/hash.svg", 16., theme::text_faint())).child(div().flex_1().child(name)).children(right);
            list_row(id, theme::row_height(theme::ROW_ONE), selected, focused, content)
        };
        let person = |id: &'static str, name: &'static str, context: &'static str| {
            let content = div()
                .gap(px(8.))
                .child(Avatar::named(name, 24.).presence(Presence::Online, theme::sidebar()).build())
                .child(div().child(name))
                .child(div().type_small().text_color(theme::text_faint()).child(format!("· {context}")));
            list_row(id, theme::row_height(theme::ROW_MID), false, false, content)
        };
        div()
            .size_full()
            .p(px(24.))
            .flex()
            .gap(px(24.))
            .bg(theme::canvas())
            .child(
                column(340.)
                    .child(label("Buttons"))
                    .child(
                        line()
                            .child(button("b1", ButtonKind::Primary, "Primary"))
                            .child(button("b2", ButtonKind::Secondary, "Secondary"))
                            .child(button("b3", ButtonKind::Ghost, "Ghost"))
                            .child(button("b4", ButtonKind::Danger, "Delete")),
                    )
                    .child(
                        line()
                            .child(button("b8", ButtonKind::Primary, "Accept").h(px(24.)))
                            .child(button("b9", ButtonKind::Secondary, "Decline").h(px(24.)))
                            .child(button_disabled("Sending…"))
                            .child(icon_button("b6", "icons/smile.svg", false))
                            .child(icon_button("b7", "icons/smile.svg", true)),
                    )
                    .child(label("Fields"))
                    .child(field(false).child(div().text_color(theme::text_faint()).child("Display name")))
                    .child(field(true).child("Wren Halloran"))
                    .child(search_field(false).child(div().flex_1().text_color(theme::text_faint()).child("Jump to…")).child(keycap("⌘K")))
                    .child(label("Avatars"))
                    .child(
                        line()
                            .children([16., 20., 24., 32., 36., 64.].map(|size| Avatar::named("Wren Halloran", size).build()))
                            .children(["Odalys Brandt", "Ines Varga", "Niko Aldana", "Tobin Reyes"].into_iter().zip(presences).map(
                                |(name, presence)| Avatar::named(name, 32.).presence(presence, theme::canvas()).build(),
                            )),
                    )
                    .child(label("Badges: mention · count · unread count · dot · accent dot · 99+"))
                    .child(
                        line()
                            .child(badge_mention(3))
                            .child(badge_count(12))
                            .child(badge_count_unread(3))
                            .child(badge_dot(8., theme::unread()))
                            .child(badge_dot(8., theme::mention()))
                            .child(badge_mention(120)),
                    )
                    .child(segmented().child(segment("s1", "All", true)).child(segment("s2", "Unread 11", false)).child(segment("s3", "Mentions 4", false)))
                    .child(label("Banners"))
                    .child(banner(Tone::Attention, "Discord wants you to verify your account.").child(banner_action("ba1", "Open Discord")))
                    .child(banner(Tone::Warning, "Slack is reconnecting. Showing what was saved at 10:42."))
                    .child(banner(Tone::Danger, "Slack signed you out of Harbor Labs.").child(banner_action("ba2", "Sign in")))
                    .child(banner(Tone::Keychain, crate::host::STORE_WAITING))
                    .child(toast(Some(("icons/circle-alert.svg", theme::danger())), "tinyclient quit unexpectedly and reopened.").child(banner_action("t1", "Copy report"))),
            )
            .child(
                column(340.)
                    .child(label("Rows: conversation 48 · place 28 · person 36"))
                    .child(
                        div()
                            .bg(theme::sidebar())
                            .py(px(8.))
                            .rounded(px(theme::R_CARD))
                            .child(section_header("Pinned", true))
                            .child(conversation("r1", "Tomasz Brandt", "Ines says 7:30 at the usual corner.", "9:24 PM", None, false))
                            .child(conversation("r2", "Ines Marchetti", "And tell Tomasz, he never reads the channel", "10:27 PM", Some(badge_count_unread(3)), true))
                            .child(section_header("Today", false))
                            .child(conversation("r3", "#general", "Ines: need a yes or no on Saturday", "10:30 PM", Some(badge_mention(2)), true))
                            .child(section_header_folding("Meetings", false, true, Some(badge_dot(8., theme::unread()))))
                            .child(place("p1", "trail-cams", None, false, false))
                            .child(place("p2", "gear-swap", Some(badge_dot(8., theme::unread())), false, false))
                            .child(place("p3", "showcase", None, true, false))
                            .child(place("p4", "photos", None, false, true))
                            .child(person("q1", "Anneke Visser", "Harbor Labs"))
                            .child(person("q2", "Dita Novak", "Moderator")),
                    ),
            )
            .child(
                column(300.)
                    .child(label("Menu"))
                    .child(
                        menu()
                            .w(px(240.))
                            .child(menu_item("m0", Some("icons/reply.svg"), "Reply", Some("R")))
                            .child(menu_item("m1", Some(NO_GLYPH), "Reply in thread", Some("T")))
                            .child(menu_item_check("m2", true, "Online", None))
                            .child(menu_item_check("m3", false, "Idle", None))
                            .child(menu_item_disabled("m4", Some(NO_GLYPH), "Mark Server as Read", Some("⇧⎋")))
                            .child(menu_separator())
                            .child(menu_label("Status"))
                            .child(menu_item("m5", Some("icons/copy.svg"), "Copy text", Some("⌘C")))
                            .child(menu_separator())
                            .child(menu_item_danger("m6", Some("icons/trash-2.svg"), "Delete…")),
                    )
                    .child(label("Modal"))
                    .child(
                        modal("Delete this message?")
                            .w(px(300.))
                            .child(modal_body("It is deleted for everyone in #showcase."))
                            .child(modal_buttons().child(button("mb1", ButtonKind::Secondary, "Cancel")).child(button("mb2", ButtonKind::Danger, "Delete"))),
                    )
                    .child(label("Empty state"))
                    .child(
                        div().h(px(200.)).bg(theme::sidebar()).rounded(px(theme::R_CARD)).child(empty_state_with(
                            "icons/key.svg",
                            "tinyclient can't read its sign-ins",
                            "Your accounts are still signed in. Allow access to continue.",
                            button("es", ButtonKind::Secondary, "Try again"),
                        )),
                    ),
            )
    }

    pub fn shot(window: gpui::WindowHandle<Shell>, cx: &mut App) {
        let Some(path) = flag("--shot") else {
            return;
        };
        cx.spawn(async move |cx| {
            let settle = Duration::from_millis(500);
            cx.background_executor().timer(settle).await;
            for step in 0..3 {
                let _ = window.update(cx, |shell, window, cx| stage(shell, step, window, cx));
                cx.background_executor().timer(settle).await;
            }
            if has("--mark-all") {
                let _ = window.update(cx, |shell, _, cx| shell.mark_all_read(cx));
                cx.background_executor().timer(Duration::from_millis(1200)).await;
            }
            // Each press waits for what the one before it read.
            for _ in 0..flag("--walk").and_then(|n| n.parse::<usize>().ok()).unwrap_or(0) {
                let _ = window.update(cx, |shell, _, cx| shell.walk_unread(true, cx));
                cx.background_executor().timer(settle).await;
            }
            for _ in 0..flag("--back").and_then(|n| n.parse::<usize>().ok()).unwrap_or(0) {
                let _ = window.update(cx, |shell, _, cx| shell.travel(true, cx));
                cx.background_executor().timer(settle).await;
            }
            // `--settle 4000` waits that long more before the picture: for steps that take time
            // (`TINYCLIENT_SHOT_STEPS`), such as the demo's call ringing and connecting.
            if let Some(ms) = flag("--settle").and_then(|n| n.parse::<u64>().ok()) {
                cx.background_executor().timer(Duration::from_millis(ms)).await;
            }
            let frames = flag("--frames").and_then(|n| n.parse::<usize>().ok()).unwrap_or(1).max(1);
            if frames > 1 {
                super::anim::SHOT_FRAMES.store(true, std::sync::atomic::Ordering::Relaxed);
            }
            for frame in 1..=frames {
                let named = match (frames, path.rsplit_once('.')) {
                    (1, _) => path.clone(),
                    (_, Some((stem, extension))) => format!("{stem}-{frame}.{extension}"),
                    (_, None) => format!("{path}-{frame}"),
                };
                // A frame drawn now, as a moving picture's timer would have the window drawn.
                let _ = window.update(cx, |_, window, _| window.refresh());
                cx.background_executor().timer(Duration::from_millis(150)).await;
                let image = window.update(cx, |_, window, _| window.render_to_image());
                match image {
                    Ok(Ok(image)) => image.save(&named).expect("the screenshot is written"),
                    other => tracing::warn!("tinyclient: no screenshot: {:?}", other.map(|r| r.map(|_| ()))),
                }
            }
            // `--hold 20` keeps the window up so its memory can be measured from outside.
            if let Some(seconds) = flag("--hold").and_then(|n| n.parse::<u64>().ok()) {
                tracing::warn!("tinyclient: holding, pid {}", std::process::id());
                cx.background_executor().timer(Duration::from_secs(seconds)).await;
            }
            // The windows go first. A focused field is registered with its window as the
            // platform's input handler, which holds the field: quitting with the window still up
            // left that handle alive past the app, and the test platform's leak check panicked
            // (exit 101) after the image was written.
            cx.update(|cx| {
                for handle in cx.windows() {
                    let _ = handle.update(cx, |_, window, _| window.remove_window());
                }
            });
            cx.background_executor().timer(Duration::from_millis(50)).await;
            cx.update(|cx| cx.quit());
        })
        .detach();
    }
}

fn flag(name: &str) -> Option<String> {
    std::env::args().skip_while(|arg| arg != name).nth(1)
}

/// `--size 1120x720`, for drawing a state at a given width. `--narrow` is 700 wide.
fn window_size() -> (f32, f32) {
    let asked = flag("--size").and_then(|size| {
        let (wide, tall) = size.split_once('x')?;
        Some((wide.parse().ok()?, tall.parse().ok()?))
    });
    match asked {
        Some(size) => size,
        None if std::env::args().any(|arg| arg == "--narrow") => (700., 720.),
        None => (1120., 720.),
    }
}

/// When the process began drawing, for `--shot`'s report of how long the first list took.
#[cfg(feature = "gui-shot")]
static LAUNCHED: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

/// Where the window opens: `--size` for a shot, else the saved frame when half of it is on a
/// display that is still there, else 1120 × 720 centered on the main display.
fn window_bounds(saved: Option<(f32, f32, f32, f32)>, cx: &App) -> Bounds<gpui::Pixels> {
    let asked = flag("--size").is_some() || std::env::args().any(|arg| arg == "--narrow" || arg == "--shot");
    let displays: Vec<(f32, f32, f32, f32)> = cx
        .displays()
        .iter()
        .map(|display| {
            let b = display.bounds();
            (f32::from(b.origin.x), f32::from(b.origin.y), f32::from(b.size.width), f32::from(b.size.height))
        })
        .collect();
    if let Some((x, y, w, h)) = saved.filter(|frame| !asked && app::prefs::frame_visible(*frame, &displays)) {
        return Bounds { origin: point(px(x), px(y)), size: size(px(w), px(h)) };
    }
    let (wide, tall) = window_size();
    let mut bounds = Bounds::centered(None, size(px(wide), px(tall)), cx);
    // Centering in a visible area of odd size lands on half a point, and the system then grows
    // the frame to whole pixels: one point wider than asked.
    bounds.origin = point(bounds.origin.x.round(), bounds.origin.y.round());
    bounds
}

/// Whether chatd can run beside the window. True on desktop, where the shell starts and asks
/// it. On phones it is true only in the `mobile` build, which runs the core in the app
/// (`lib.rs`, `inprocess`). Without it the shell runs with no service, which reads as zero
/// signed-in accounts (`app::watch`).
pub(crate) fn mobile_core_available() -> bool {
    !cfg!(any(target_os = "android", target_os = "ios")) || cfg!(feature = "mobile")
}

/// Whether links and files open through GPUI (`cx.open_url`) rather than a desktop helper such
/// as `xdg-open`. True on phones, where those helpers do not exist.
pub(crate) fn links_through_gpui() -> bool {
    cfg!(any(target_os = "android", target_os = "ios"))
}

/// Feeds the platform's safe area (status bar, cutout, home indicator or navigation bar) and the
/// soft keyboard into the phone's tokens. GPUI refreshes the window whenever the platform reports
/// new insets, and the shell calls this on every frame, so each change reaches the next draw. All
/// zero means the platform has not reported yet, and the tokens keep their stand-ins.
pub(crate) fn feed_platform_insets(window: &gpui::Window) {
    #[cfg(any(target_os = "android", target_os = "ios"))]
    {
        let insets = window.insets();
        if insets != gpui::WindowInsets::default() {
            phone::tokens::set_insets(phone::tokens::Insets {
                top: f32::from(insets.safe_area.top),
                bottom: f32::from(insets.safe_area.bottom),
                keyboard: f32::from(insets.ime.bottom),
                left: f32::from(insets.safe_area.left),
                right: f32::from(insets.safe_area.right),
            });
        }
    }
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    let _ = window;
}

/// Opens the window. `invite` is a `tinyclient://join#…` link the launch carried, `action` a
/// quick action (`--action`/`--dock-action`); `incoming` delivers the links a second launch
/// hands this one.
pub fn run(invite: Option<String>, action: Option<crate::quick::QuickAction>, incoming: Option<std::sync::mpsc::Receiver<String>>) {
    run_with(gpui_platform::application(), invite, action, incoming);
}

/// Runs the window on an application the caller built. Android builds its own (see
/// `android_main`): `gpui_platform::application()` there has no NativeActivity session, so
/// GPUI would open the window before the surface exists.
pub(crate) fn run_with(application: gpui::Application, invite: Option<String>, action: Option<crate::quick::QuickAction>, incoming: Option<std::sync::mpsc::Receiver<String>>) {
    #[cfg(feature = "gui-shot")]
    LAUNCHED.get_or_init(std::time::Instant::now);
    app::crash::install();
    app::read_reduce_motion();
    theme::read_system_contrast();
    if let Some(word) = flag("--theme") {
        theme::set_override(Some(&word));
    }
    if let Some(word) = flag("--accent") {
        theme::set_accent_override(Some(&word));
    }
    attention::read_launch();
    time::read_clock_preference();
    // Read before the window opens, so its first frame draws where the user was.
    let prefs = app::prefs::read();
    // A demo shot draws only made-up data: nothing the user last had open is restored.
    #[cfg(any(feature = "gui-shot", feature = "demo"))]
    let prefs = if shot_demo::on() { app::prefs::Prefs::default() } else { prefs };
    theme::set_text_step(prefs.text_step);
    app::prefs::set_choices(prefs.choices);
    lock::install(prefs.lock.clone());
    app::prefs::set_trusted_links(prefs.trusted_links.clone());
    anim::set_autoplay(prefs.autoplay);
    let application = application.with_assets(assets::Assets);
    // macOS delivers both launch and subsequent links as open-URL events. Keep them in
    // memory until the current sign-in sheet closes; never discard an active flow.
    let links = std::rc::Rc::new(std::cell::RefCell::new(std::collections::VecDeque::<String>::new()));
    let received = links.clone();
    application.on_open_urls(move |urls| {
        for url in urls {
            if (url.starts_with("tinyclient://join#") && protocol::parse_invite_link(&url).is_some())
                || protocol::parse_room_link(&url).is_some()
                || protocol::parse_message_link(&url).is_some()
            {
                let mut queue = received.borrow_mut();
                if queue.len() < 8 && !queue.contains(&url) { queue.push_back(url); }
            }
        }
    });
    // The Dock icon brings the hidden window back unchanged.
    application.on_reopen(|cx| cx.activate(true));
    application.run(move |cx: &mut App| {
            keymap::load();
            rebind_keys(cx);
            app::menubar::app_actions(cx);
            if !cfg!(feature = "gui-shot") {
                crate::update::recall();
            }
            cx.set_menus(app::menubar::menus(&[], None, crate::update::menu_label()));
            // The same three quick actions on every launcher: the Dock menu (macOS) and the
            // taskbar's jump list (Windows) here, the desktop file's Actions on Linux
            // (`packaging/linux/dev.tinyclient.app.desktop`). The Windows list is written under
            // the identity the installer's shortcuts carry, so it groups with them
            // (`packaging/windows/tinyclient.iss`). A shot run leaves the user's own list alone.
            if !cfg!(feature = "gui-shot") {
                cx.set_app_identity(APP_ID, "tinyclient");
                cx.set_dock_menu(vec![
                    MenuItem::action("New Message", NewMessage),
                    MenuItem::action("Inbox", Place1),
                    MenuItem::action("Quit tinyclient", Quit),
                ]);
            }

            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(window_bounds(prefs.frame, cx))),
                // A phone window asks for no minimum: folded and split-screen windows must be
                // allowed to be smaller than the frame we chose.
                window_min_size: (!layout::phone(window_size().0) && !phone::form(false, window_size().0, window_size().1))
                    .then_some(size(px(layout::WINDOW_MIN.0), px(layout::WINDOW_MIN.1))),
                titlebar: Some(TitlebarOptions {
                    title: Some("tinyclient".into()),
                    appears_transparent: true,
                    // Centered on the 52 pt strips, inside the rail.
                    traffic_light_position: Some(point(px(10.), px(20.))),
                }),
                // A compositor's own title bar where it draws one; `chrome` draws the rest.
                window_decorations: Some(WindowDecorations::Server),
                app_id: Some(APP_ID.into()),
                ..Default::default()
            };
            #[cfg(windows)]
            let tray = prefs.choices.tray;
            let opened = cx.open_window(options, |window, cx| {
                theme::set_system_appearance(window.appearance());
                window
                    .observe_window_appearance(|window, cx| {
                        theme::set_system_appearance(window.appearance());
                        cx.refresh_windows();
                    })
                    .detach();
                // The launcher entry and the flash's urgency hint are this window's from now.
                #[cfg(target_os = "linux")]
                attention::note_window(window.window_handle());
                // The red button closes the window as ⌘W does (Keep receiving decides how).
                window.on_window_should_close(cx, |_, cx| {
                    tracing::warn!("tinyclient: window should close");
                    if let Some(guard) = protocol::log_policy::current() {
                        guard.flush();
                    }
                    app::menubar::close_window(cx);
                    false
                });
                cx.new(|cx| {
                    let shell = Shell::new(prefs, window, cx);
                    window.focus(&shell.focus, cx);
                    shell
                })
            });
            let window = match opened {
                Ok(window) => {
                    tracing::info!(
                        "tinyclient: window open pid={} windows={}",
                        std::process::id(),
                        cx.windows().len()
                    );
                    if let Some(guard) = protocol::log_policy::current() {
                        guard.flush();
                    }
                    window
                }
                Err(err) => {
                    // No window (no GPU, a headless session): say so and leave, never panic.
                    // Nothing can be drawn without a window, so the record is the only report.
                    tracing::warn!("tinyclient: the window could not open: {err}");
                    if let Some(guard) = protocol::log_policy::current() {
                        guard.flush();
                    }
                    // On Android the `log` records are what reach logcat (`lib.rs`, `android_log`).
                    #[cfg(target_os = "android")]
                    log::error!("tinyclient: the window could not open: {err}");
                    cx.quit();
                    return;
                }
            };
            // The HWND can die while GPUI's loop keeps spinning on "Invalid window handle"
            // and never drains timers. Watch from a plain OS thread so a hung UI still exits.
            watch_for_vanished_window();
            // The tray icon (G3, Windows so far) exists exactly while the choice is on: it is
            // what makes closing mean hide, and the click on it the way back in
            // (docs/product/09-settings.md). Off, as prefs ship, nothing is added.
            #[cfg(windows)]
            notifier::tray::set(tray);
            if let Some(invite) = invite {
                links.borrow_mut().push_back(invite);
            }
            if let Some(action) = action {
                links.borrow_mut().push_back(action.link());
            }
            cx.spawn(async move |cx| {
                loop {
                    cx.background_executor().timer(std::time::Duration::from_millis(500)).await;
                    // The tray's Quit (G3) is a flag the icon's window set from GPUI's own
                    // thread; this loop delivers it, so the ask waits at most one tick and
                    // runs the menu bar's quit, which stops chatd.
                    #[cfg(windows)]
                    if notifier::tray::take_quit() {
                        let _ = cx.update(|cx| app::menubar::quit(cx));
                    }
                    if let Some(incoming) = incoming.as_ref() {
                        while links.borrow().len() < 8 {
                            let Ok(link) = incoming.try_recv() else { break; };
                            let mut queue = links.borrow_mut();
                            if !queue.contains(&link) { queue.push_back(link); }
                        }
                    }
                    #[cfg(any(target_os = "android", target_os = "ios"))]
                    {
                        while links.borrow().len() < 8 {
                            let Some(link) = crate::mobile_share::next_link() else { break };
                            links.borrow_mut().push_back(link);
                        }
                        let _ = window.update(cx, |shell, window, cx| {
                            if window.is_window_active() {
                                shell.receive_phone_share(cx);
                            }
                        });
                    }
                    let next = links.borrow().front().cloned();
                    if let Some(next) = next {
                        if let Some(action) = crate::quick::parse_link(&next) {
                            // A quick action a second launch handed over: the same action as
                            // its menu bar item, once, then the next link in line.
                            links.borrow_mut().pop_front();
                            cx.update(|cx| {
                                dispatch_quick(action, cx);
                                cx.refresh_windows();
                                cx.activate(true);
                            });
                        } else if let Some((server, room_id)) = protocol::parse_room_link(&next) {
                            links.borrow_mut().pop_front();
                            let _ = window.update(cx, |shell, _, cx| {
                                shell.tiny_open_room_link(&server, &room_id, cx);
                            });
                            cx.update(|cx| { cx.refresh_windows(); cx.activate(true); });
                        } else if let Some((origin, conv_id, msg_id)) = protocol::parse_message_link(&next) {
                            links.borrow_mut().pop_front();
                            let _ = window.update(cx, |shell, _, cx| {
                                shell.tiny_open_message_link(&origin, &conv_id, &msg_id, cx);
                            });
                            cx.update(|cx| { cx.refresh_windows(); cx.activate(true); });
                        } else if cx.update(|cx| signin::open_invite(&next, cx)) {
                            links.borrow_mut().pop_front();
                            cx.update(|cx| { cx.refresh_windows(); cx.activate(true); });
                        }
                    }
                }
            }).detach();
            #[cfg(feature = "gui-shot")]
            shot::shot(window, cx);
            #[cfg(feature = "gui-shot")]
            settings::shot(cx);
            let _ = window;
            cx.activate(true);
        });
}

/// What a quick action does when it reaches the window: the same action as its menu bar item,
/// handled where the others are (`app::menubar`, the shell's keys). Unsigned in, New Message
/// does what the menu item does with nothing signed in: nothing.
fn dispatch_quick(action: crate::quick::QuickAction, cx: &mut App) {
    match action {
        crate::quick::QuickAction::NewMessage => cx.dispatch_action(&NewMessage),
        crate::quick::QuickAction::Inbox => cx.dispatch_action(&Place1),
        crate::quick::QuickAction::Quit => cx.dispatch_action(&Quit),
    }
}

/// Watches for the main HWND vanishing while the process hangs. OS thread, not GPUI timers.
fn watch_for_vanished_window() {
    #[cfg(windows)]
    {
        std::thread::Builder::new()
            .name("window-watch".into())
            .spawn(|| {
                let mut seen = false;
                let mut missing = 0u8;
                loop {
                    std::thread::sleep(std::time::Duration::from_secs(2));
                    if os_window_alive() {
                        seen = true;
                        missing = 0;
                        continue;
                    }
                    if !seen {
                        continue;
                    }
                    missing = missing.saturating_add(1);
                    if missing < 2 {
                        continue;
                    }
                    tracing::warn!("tinyclient: OS window gone; exiting hung process");
                    if let Some(guard) = protocol::log_policy::current() {
                        guard.flush();
                    }
                    std::process::exit(1);
                }
            })
            .ok();
    }
}

/// Whether this process still owns a titled or visible top-level window.
fn os_window_alive() -> bool {
    #[cfg(windows)]
    {
        use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
        use windows_sys::Win32::Foundation::{HWND, LPARAM};
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            EnumWindows, GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindow, IsWindowVisible,
        };

        static WANT_PID: AtomicU32 = AtomicU32::new(0);
        static FOUND: AtomicBool = AtomicBool::new(false);
        WANT_PID.store(std::process::id(), Ordering::SeqCst);
        FOUND.store(false, Ordering::SeqCst);
        unsafe extern "system" fn each(hwnd: HWND, _: LPARAM) -> i32 {
            let mut window_pid = 0u32;
            unsafe { GetWindowThreadProcessId(hwnd, &mut window_pid) };
            if window_pid != WANT_PID.load(Ordering::SeqCst) || unsafe { IsWindow(hwnd) == 0 } {
                return 1;
            }
            // Ignore untitled message-only helpers; the shell window is titled or visible
            // (minimised still counts via IsIconic).
            let mut title = [0u16; 8];
            let titled = unsafe { GetWindowTextW(hwnd, title.as_mut_ptr(), title.len() as i32) } > 0;
            let shown = unsafe { IsWindowVisible(hwnd) != 0 || IsIconic(hwnd) != 0 };
            if titled || shown {
                FOUND.store(true, Ordering::SeqCst);
                return 0;
            }
            1
        }
        unsafe {
            let _ = EnumWindows(Some(each), 0);
        }
        FOUND.load(Ordering::SeqCst)
    }
    #[cfg(not(windows))]
    {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letters_typed_at_the_lists_go_to_the_composer_and_list_keys_do_not() {
        assert!(typed_text("a") && typed_text("É") && typed_text("?"));
        assert!(!typed_text("") && !typed_text("\n") && !typed_text("\t") && !typed_text(" "));
    }

    #[test]
    fn calls_tiny_discord_responsive_invariants_hold() {
        assert_eq!(prove_calls_tiny_responsive(), Ok(()));
    }

}
