//! Slash-command completion and argument parsing for `/` at the start of the composer.

use crate::gui::components::menu;
use crate::gui::theme::{self, Type};
use gpui::{div, prelude::*, px, AnyElement, App, SharedString, Window};
use protocol::{AppCommand, CommandArg, CommandChoice, CommandOption, CommandOptionKind};

/// Maximum commands shown at once in the `/` popover.
pub(super) const ROWS: usize = 8;

/// Open `/` popover state: either picking a command or picking an autocomplete choice for an option.
#[derive(Clone, Debug)]
pub(super) enum Mode {
    Commands(Vec<AppCommand>),
    Choices {
        command: AppCommand,
        option: String,
        replace_start: usize,
        choices: Vec<CommandChoice>,
    },
}

#[derive(Clone, Debug)]
pub(super) struct List {
    pub mode: Mode,
    pub selected: usize,
}

impl List {
    pub fn len(&self) -> usize {
        match &self.mode {
            Mode::Commands(cmds) => cmds.len(),
            Mode::Choices { choices, .. } => choices.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn step(&mut self, down: bool) {
        let len = self.len();
        if len == 0 {
            self.selected = 0;
            return;
        }
        if down {
            self.selected = (self.selected + 1) % len;
        } else {
            self.selected = self.selected.checked_sub(1).unwrap_or(len - 1);
        }
    }
}

/// Returns the partial command query when the caret is typing `/name` at byte 0 (before any space).
pub(super) fn command_trigger(text: &str, caret: usize) -> Option<&str> {
    if !text.starts_with('/') || caret == 0 || caret > text.len() || !text.is_char_boundary(caret) {
        return None;
    }
    let prefix = &text[1..caret];
    if prefix.contains(char::is_whitespace) {
        return None;
    }
    Some(prefix)
}

/// Active option being typed after `/cmd `, returning `(command_name, option_name, typed_value, replace_start, filled_args)`.
pub(super) struct ActiveOption {
    pub command: AppCommand,
    pub option: CommandOption,
    pub typed: String,
    pub replace_start: usize,
    pub filled: Vec<CommandArg>,
}

/// Detects when the caret is inside an argument of a known slash command that supports
/// static choices or live `autocomplete`.
pub(super) fn active_option(text: &str, caret: usize, commands: &[AppCommand]) -> Option<ActiveOption> {
    if !text.starts_with('/') || caret > text.len() || !text.is_char_boundary(caret) {
        return None;
    }
    let before = &text[1..caret];
    let space = before.find(char::is_whitespace)?;
    let cmd_name = &before[..space];
    let cmd = commands.iter().find(|c| c.name.eq_ignore_ascii_case(cmd_name))?;
    if cmd.options.is_empty() {
        return None;
    }
    let rest_start = 1 + space + 1;
    let rest = text.get(rest_start..caret).unwrap_or("");
    // A subcommand or group is not the value being typed. Step into it once its name is complete.
    let (level, args_text) = leaf_span(rest, &cmd.options);
    if level.is_empty() || level.iter().all(is_branch) {
        return None;
    }
    let args_off = rest.len() - args_text.len();
    let filled = parse_args(rest, &cmd.options);
    // Find the last whitespace-delimited token currently at the caret.
    let token_rel = args_text.rfind(char::is_whitespace).map(|i| i + 1).unwrap_or(0);
    let token = &args_text[token_rel..];
    let (opt, typed, replace_start) = if let Some((k, v)) = token.split_once(':') {
        let opt = level.iter().find(|o| !is_branch(o) && o.name.eq_ignore_ascii_case(k))?.clone();
        (opt, v.to_owned(), rest_start + args_off + token_rel + k.len() + 1)
    } else {
        // Positional: pick the option matching the count of already-completed tokens before this one.
        let completed = args_text[..token_rel].split_whitespace().count();
        let values: Vec<&CommandOption> = level.iter().filter(|opt| !is_branch(opt)).collect();
        let opt = values.get(completed).or_else(|| values.last()).map(|opt| (*opt).clone())?;
        (opt, token.to_owned(), rest_start + args_off + token_rel)
    };
    if !opt.autocomplete && opt.choices.is_empty() {
        return None;
    }
    Some(ActiveOption {
        command: cmd.clone(),
        option: opt,
        typed,
        replace_start,
        filled,
    })
}

/// Filters `commands` matching `query` (prefix matches first, then substring matches).
pub(super) fn filter(commands: &[AppCommand], query: &str) -> Vec<AppCommand> {
    let q = query.trim().to_ascii_lowercase();
    let mut prefix = Vec::new();
    let mut contains = Vec::new();
    for cmd in commands {
        let name = cmd.name.to_ascii_lowercase();
        let desc = cmd.description.to_ascii_lowercase();
        if q.is_empty() || name.starts_with(&q) {
            prefix.push(cmd.clone());
        } else if name.contains(&q) || desc.contains(&q) {
            contains.push(cmd.clone());
        }
    }
    prefix.extend(contains);
    prefix.truncate(ROWS);
    prefix
}

/// Filters static `choices` by `typed` substring.
pub(super) fn filter_choices(choices: &[CommandChoice], typed: &str) -> Vec<CommandChoice> {
    let q = typed.trim().to_ascii_lowercase();
    choices
        .iter()
        .filter(|c| q.is_empty() || c.name.to_ascii_lowercase().contains(&q) || c.value.to_ascii_lowercase().contains(&q))
        .take(ROWS)
        .cloned()
        .collect()
}

/// Parses `/name ...` into a matching `AppCommand` and its typed arguments.
pub(super) fn parse_invocation<'a>(text: &str, commands: &'a [AppCommand]) -> Option<(&'a AppCommand, Vec<CommandArg>)> {
    let trimmed = text.trim();
    let rest = trimmed.strip_prefix('/')?;
    if rest.is_empty() {
        return None;
    }
    let (cmd_name, tail) = match rest.find(char::is_whitespace) {
        Some(ix) => (&rest[..ix], rest[ix..].trim()),
        None => (rest, ""),
    };
    let cmd = commands.iter().find(|c| c.name.eq_ignore_ascii_case(cmd_name))?;
    let args = parse_args(tail, &cmd.options);
    Some((cmd, args))
}

fn is_branch(opt: &CommandOption) -> bool {
    matches!(opt.kind, CommandOptionKind::SubCommand | CommandOptionKind::SubCommandGroup)
}

/// The option list the caret is filling, and the unread suffix of `rest` after any finished
/// subcommand or group name. A name still being typed stays at this level.
fn leaf_span<'a>(rest: &'a str, options: &'a [CommandOption]) -> (&'a [CommandOption], &'a str) {
    if !options.iter().any(is_branch) {
        return (options, rest);
    }
    let skipped = rest.len() - rest.trim_start().len();
    let trimmed = &rest[skipped..];
    let Some((token, consumed)) = head_span(trimmed) else {
        return (options, rest);
    };
    let Some(opt) = options.iter().find(|opt| is_branch(opt) && opt.name.eq_ignore_ascii_case(&token)) else {
        return (options, rest);
    };
    let after = &trimmed[consumed..];
    if after.starts_with(char::is_whitespace) {
        leaf_span(&rest[skipped + consumed..], &opt.options)
    } else {
        (options, rest)
    }
}

/// First token, with its quotes removed, and how many bytes of `tail` it occupied.
fn head_span(tail: &str) -> Option<(String, usize)> {
    if tail.is_empty() {
        return None;
    }
    let mut consumed = 0usize;
    let mut token = String::new();
    let mut in_quotes = false;
    for ch in tail.chars() {
        let len = ch.len_utf8();
        if ch == '"' {
            in_quotes = !in_quotes;
            consumed += len;
            continue;
        }
        if ch.is_whitespace() && !in_quotes {
            break;
        }
        token.push(ch);
        consumed += len;
    }
    (!token.is_empty()).then_some((token, consumed))
}

fn head(tail: &str) -> Option<(String, &str)> {
    let tail = tail.trim_start();
    let (token, consumed) = head_span(tail)?;
    Some((token, &tail[consumed..]))
}

/// A subcommand or group name, then whatever was typed inside it.
/// The name kept is the one on the command definition.
fn parse_branch(tail: &str, options: &[CommandOption]) -> Vec<CommandArg> {
    let Some((token, after)) = head(tail) else {
        return Vec::new();
    };
    let Some(opt) = options.iter().find(|opt| is_branch(opt) && opt.name.eq_ignore_ascii_case(&token)) else {
        return Vec::new();
    };
    let mut out = vec![CommandArg { name: opt.name.clone(), value: String::new(), kind: opt.kind }];
    out.extend(parse_args(after, &opt.options));
    out
}

/// Parses `key:value` pairs and positional arguments against `options`.
/// Subcommands and groups nest: the next token picks one, and the rest is parsed inside it.
pub(super) fn parse_args(tail: &str, options: &[CommandOption]) -> Vec<CommandArg> {
    let tail = tail.trim();
    if tail.is_empty() {
        return Vec::new();
    }
    if options.iter().any(is_branch) {
        return parse_branch(tail, options);
    }
    // Single-option commands take the entire tail verbatim unless explicitly named `opt:val`.
    if options.len() == 1 {
        let opt = &options[0];
        let val = tail
            .strip_prefix(&format!("{}:", opt.name))
            .map(str::trim)
            .unwrap_or(tail);
        return vec![CommandArg { name: opt.name.clone(), value: val.to_owned(), kind: opt.kind }];
    }
    let mut out: Vec<CommandArg> = Vec::new();
    let mut positional_idx = 0usize;
    let tokens = split_tokens(tail);
    for tok in tokens {
        if let Some((k, v)) = tok.split_once(':') {
            if let Some(opt) = options.iter().find(|o| o.name.eq_ignore_ascii_case(k)) {
                out.push(CommandArg { name: opt.name.clone(), value: v.to_owned(), kind: opt.kind });
                continue;
            }
        }
        if let Some(opt) = options.get(positional_idx) {
            positional_idx += 1;
            out.push(CommandArg { name: opt.name.clone(), value: tok, kind: opt.kind });
        } else if let Some(last) = out.last_mut() {
            if last.kind == CommandOptionKind::String {
                last.value.push(' ');
                last.value.push_str(&tok);
            }
        }
    }
    out
}

/// Splits whitespace-separated tokens, respecting `"quoted values"` (including `key:"quoted value"`).
fn split_tokens(input: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    for ch in input.chars() {
        match ch {
            '"' => in_quotes = !in_quotes,
            c if c.is_whitespace() && !in_quotes => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Formats a command's option signature, e.g. `<env> [note]`.
pub(super) fn signature(options: &[CommandOption]) -> String {
    options
        .iter()
        .take(4)
        .map(|opt| {
            if opt.required {
                format!("<{}>", opt.name)
            } else {
                format!("[{}]", opt.name)
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Draws the slash-command or option-autocomplete popover above the composer.
pub(super) fn draw<F>(list: &List, pick: F) -> AnyElement
where
    F: Fn(usize, &mut Window, &mut App) + Clone + 'static,
{
    let mut panel = menu().w_full().max_w(px(360.)).flex().flex_col().gap(px(2.));
    match &list.mode {
        Mode::Commands(commands) => {
            for (ix, cmd) in commands.iter().enumerate() {
                let selected = ix == list.selected;
                let sig = signature(&cmd.options);
                let pick = pick.clone();
                let mut head = div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .min_w_0()
                    .child(
                        div()
                            .flex_none()
                            .max_w(px(160.))
                            .min_w_0()
                            .truncate()
                            .type_small()
                            .font_weight(theme::SEMIBOLD)
                            .text_color(theme::text())
                            .child(format!("/{}", cmd.name)),
                    );
                if !sig.is_empty() {
                    head = head.child(div().flex_1().min_w_0().truncate().type_micro().text_color(theme::text_faint()).child(sig));
                }
                if !cmd.app_name.is_empty() {
                    head = head.child(
                        div()
                            .ml_auto()
                            .flex_none()
                            .max_w(px(100.))
                            .min_w_0()
                            .truncate()
                            .type_micro()
                            .text_color(theme::text_faint())
                            .child(cmd.app_name.clone()),
                    );
                }
                let mut row = div()
                    .id(SharedString::from(format!("slash-cmd-{ix}")))
                    .px(px(8.))
                    .py(px(6.))
                    .rounded(px(theme::R_CONTROL))
                    .cursor_pointer()
                    .when(selected, |d| d.bg(theme::selected()))
                    .when(!selected, |d| d.hover(|s| s.bg(theme::hover())))
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .on_mouse_down(gpui::MouseButton::Left, move |_, window, cx| {
                        cx.stop_propagation();
                        window.prevent_default();
                        pick(ix, window, cx);
                    })
                    .child(head);
                if !cmd.description.is_empty() {
                    row = row.child(
                        div()
                            .type_micro()
                            .text_color(theme::text_soft())
                            .truncate()
                            .child(cmd.description.clone()),
                    );
                }
                panel = panel.child(row);
            }
        }
        Mode::Choices { option, choices, .. } => {
            panel = panel.child(
                div()
                    .px(px(8.))
                    .py(px(3.))
                    .type_micro()
                    .text_color(theme::text_faint())
                    .child(format!("Suggestions for {option}")),
            );
            for (ix, choice) in choices.iter().enumerate() {
                let selected = ix == list.selected;
                let pick = pick.clone();
                panel = panel.child(
                    div()
                        .id(SharedString::from(format!("slash-choice-{ix}")))
                        .px(px(8.))
                        .py(px(5.))
                        .rounded(px(theme::R_CONTROL))
                        .cursor_pointer()
                        .when(selected, |d| d.bg(theme::selected()))
                        .when(!selected, |d| d.hover(|s| s.bg(theme::hover())))
                        .min_w_0()
                        .truncate()
                        .type_small()
                        .text_color(theme::text())
                        .on_mouse_down(gpui::MouseButton::Left, move |_, window, cx| {
                            cx.stop_propagation();
                            window.prevent_default();
                            pick(ix, window, cx);
                        })
                        .child(choice.name.clone()),
                );
            }
        }
    }
    panel.id("composer-app-slash-scroll").max_h(px(260.)).overflow_y_scroll().into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deploy_cmd() -> AppCommand {
        AppCommand {
            id: "1".into(),
            application_id: "bot".into(),
            version: "v1".into(),
            kind: protocol::CommandKind::ChatInput,
            name: "deploy".into(),
            description: "Deploy a service".into(),
            app_name: "DeployBot".into(),
            options: vec![
                CommandOption {
                    name: "env".into(),
                    description: "Target".into(),
                    kind: CommandOptionKind::String,
                    required: true,
                    autocomplete: true,
                    ..Default::default()
                },
                CommandOption {
                    name: "reason".into(),
                    description: "Why".into(),
                    kind: CommandOptionKind::String,
                    required: false,
                    ..Default::default()
                },
            ],
            built_in: false,
        }
    }

    #[test]
    fn command_trigger_matches_leading_slash_only() {
        assert_eq!(command_trigger("/", 1), Some(""));
        assert_eq!(command_trigger("/dep", 4), Some("dep"));
        assert_eq!(command_trigger("/dep staging", 12), None);
        assert_eq!(command_trigger("hi /dep", 7), None);
    }

    #[test]
    fn parse_invocation_reads_named_and_positional_args() {
        let cmds = [deploy_cmd()];
        let (cmd, args) = parse_invocation("/deploy env:staging reason:\"hotfix 1\"", &cmds).unwrap();
        assert_eq!(cmd.name, "deploy");
        assert_eq!(args.len(), 2);
        assert_eq!((args[0].name.as_str(), args[0].value.as_str()), ("env", "staging"));
        assert_eq!((args[1].name.as_str(), args[1].value.as_str()), ("reason", "hotfix 1"));

        let (_, pos) = parse_invocation("/deploy prod urgent deploy", &cmds).unwrap();
        assert_eq!((pos[0].name.as_str(), pos[0].value.as_str()), ("env", "prod"));
        assert_eq!((pos[1].name.as_str(), pos[1].value.as_str()), ("reason", "urgent deploy"));
    }

    #[test]
    fn active_option_detects_autocomplete_target() {
        let cmds = [deploy_cmd()];
        let active = active_option("/deploy sta", 11, &cmds).unwrap();
        assert_eq!(active.option.name, "env");
        assert_eq!(active.typed, "sta");
        assert_eq!(active.replace_start, 8);

        let named = active_option("/deploy env:st", 14, &cmds).unwrap();
        assert_eq!(named.option.name, "env");
        assert_eq!(named.typed, "st");
        assert_eq!(named.replace_start, 12);
    }

    fn mod_cmd() -> AppCommand {
        AppCommand {
            name: "mod".into(),
            options: vec![CommandOption {
                name: "user".into(),
                kind: CommandOptionKind::SubCommandGroup,
                options: vec![CommandOption {
                    name: "grant".into(),
                    kind: CommandOptionKind::SubCommand,
                    options: vec![CommandOption {
                        name: "perm".into(),
                        kind: CommandOptionKind::String,
                        required: true,
                        autocomplete: true,
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn parse_invocation_nests_subcommand_groups() {
        let cmds = [mod_cmd()];
        let (_, args) = parse_invocation("/mod USER grant perm:admin", &cmds).unwrap();
        assert_eq!(args.len(), 3);
        assert_eq!((args[0].name.as_str(), args[0].kind), ("user", CommandOptionKind::SubCommandGroup));
        assert_eq!((args[1].name.as_str(), args[1].kind), ("grant", CommandOptionKind::SubCommand));
        assert_eq!((args[2].name.as_str(), args[2].value.as_str(), args[2].kind), ("perm", "admin", CommandOptionKind::String));

        // The only top-level option is a subcommand, so the tail is not stuffed into it.
        let mut ban = AppCommand { name: "tool".into(), ..Default::default() };
        ban.options = vec![CommandOption {
            name: "ban".into(),
            kind: CommandOptionKind::SubCommand,
            options: vec![CommandOption { name: "reason".into(), kind: CommandOptionKind::String, ..Default::default() }],
            ..Default::default()
        }];
        let (_, args) = parse_invocation("/tool ban they were rude", &[ban]).unwrap();
        assert_eq!((args[0].name.as_str(), args[0].value.as_str(), args[0].kind), ("ban", "", CommandOptionKind::SubCommand));
        assert_eq!((args[1].name.as_str(), args[1].value.as_str()), ("reason", "they were rude"));
    }

    #[test]
    fn active_option_steps_into_a_subcommand() {
        let cmds = [mod_cmd()];
        let text = "/mod user grant pe";
        let active = active_option(text, text.len(), &cmds).unwrap();
        assert_eq!(active.option.name, "perm");
        assert_eq!(active.typed, "pe");
        assert_eq!(active.filled[0].name, "user");
        assert_eq!(active.filled[1].name, "grant");
    }

    #[test]
    fn a_single_string_option_still_takes_the_tail() {
        let cmd = AppCommand {
            name: "shrug".into(),
            options: vec![CommandOption { name: "text".into(), kind: CommandOptionKind::String, ..Default::default() }],
            ..Default::default()
        };
        let (_, args) = parse_invocation("/shrug hello there", &[cmd]).unwrap();
        assert_eq!((args[0].name.as_str(), args[0].value.as_str()), ("text", "hello there"));
    }
}
