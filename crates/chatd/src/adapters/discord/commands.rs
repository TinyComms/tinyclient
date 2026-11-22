//! Slash commands and context menus. The index is Discord's application-command index
//! (`applications`, `application_commands`, `version`). Invoking posts `POST /interactions`
//! once (type 2, or type 4 for autocomplete) and never retries. Autocomplete choices arrive
//! on gateway `APPLICATION_COMMAND_AUTOCOMPLETE_RESPONSE`, matched by nonce.

use super::interactions::snowflake_nonce;
use super::{refusal, Discord};
use crate::blobs::upload::Upload;
use protocol::{
    AppCommand, CommandArg, CommandChoice, CommandKind, CommandOption, CommandOptionKind, ConversationKind,
    OutgoingFile, COMMAND_CHOICES_LIMIT, COMMAND_OPTIONS_LIMIT, COMMANDS_LIMIT,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::mpsc;
use std::time::Duration;

const AUTOCOMPLETE_WAIT: Duration = Duration::from_secs(3);
const BUILTIN_PREFIX: &str = "builtin:";
const SHRUG: &str = "¯\\_(ツ)_/¯";
const TABLEFLIP: &str = "(╯°□°）╯︵ ┻━┻";
const UNFLIP: &str = "┬─┬ ノ( ゜-゜ノ)";

#[derive(Deserialize)]
struct Index {
    #[serde(default)]
    applications: Vec<IndexApp>,
    #[serde(default)]
    application_commands: Vec<IndexCommand>,
}

#[derive(Deserialize)]
struct IndexApp {
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
}

#[derive(Deserialize)]
struct IndexCommand {
    #[serde(default)]
    id: String,
    #[serde(default)]
    application_id: String,
    #[serde(default)]
    version: String,
    #[serde(rename = "type", default)]
    kind: u8,
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    options: Vec<IndexOption>,
}

#[derive(Deserialize)]
struct IndexOption {
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: String,
    #[serde(rename = "type", default)]
    kind: u8,
    #[serde(default)]
    required: bool,
    #[serde(default)]
    autocomplete: bool,
    #[serde(default)]
    choices: Vec<IndexChoice>,
    #[serde(default)]
    options: Vec<IndexOption>,
}

#[derive(Deserialize)]
struct IndexChoice {
    #[serde(default)]
    name: String,
    #[serde(default)]
    value: Value,
}

#[derive(Deserialize)]
struct AutocompleteEvent {
    #[serde(default)]
    choices: Vec<IndexChoice>,
    #[serde(default)]
    nonce: Option<String>,
}

impl Discord {
    pub(super) fn commands(&self, channel: &str, query: &str, kind: Option<CommandKind>) -> Result<Vec<AppCommand>, super::super::Refusal> {
        let conversation = self
            .ctx
            .known(channel)
            .map_err(|err| super::super::Refusal::Refused(err.to_string()))?
            .ok_or_else(|| super::super::Refusal::Refused("This conversation is gone.".to_owned()))?;
        let guild = self.guild_of(&conversation);
        self.ensure_index(channel, guild.as_deref())?;
        let want = kind.unwrap_or(CommandKind::ChatInput);
        let mut listed = Vec::new();
        if want == CommandKind::ChatInput {
            listed.extend(builtins());
        }
        listed.extend(self.cached(channel, guild.as_deref()).into_iter().filter(|cmd| cmd.kind == want));
        let query = query.trim().to_ascii_lowercase();
        if !query.is_empty() {
            listed.retain(|cmd| cmd.name.to_ascii_lowercase().contains(&query));
        }
        listed.truncate(COMMANDS_LIMIT);
        Ok(listed)
    }

    /// shortcut: the index stays for this session, refresh on GUILD_APPLICATION_COMMAND_INDEX_UPDATE.
    fn ensure_index(&self, channel: &str, guild: Option<&str>) -> Result<(), super::super::Refusal> {
        for (key, path, required) in sources(channel, guild) {
            if self.state.lock().unwrap().command_index.contains_key(&key) {
                continue;
            }
            match self.fetch_index(&path) {
                Ok(commands) => {
                    self.state.lock().unwrap().command_index.insert(key, commands);
                }
                Err(why) if required => return Err(super::super::Refusal::Refused(why)),
                Err(why) => self.log("commands", why),
            }
        }
        Ok(())
    }

    fn fetch_index(&self, path: &str) -> Result<Vec<AppCommand>, String> {
        let body = self.call("GET", path, None).map_err(|err| err.reason())?;
        parse_index(&body)
    }

    fn cached(&self, channel: &str, guild: Option<&str>) -> Vec<AppCommand> {
        let state = self.state.lock().unwrap();
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        for (key, _, _) in sources(channel, guild) {
            for cmd in state.command_index.get(&key).into_iter().flatten() {
                if seen.insert(cmd.id.clone()) {
                    out.push(cmd.clone());
                }
            }
        }
        out
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn command_autocomplete(
        &self,
        channel: &str,
        command_id: &str,
        application_id: &str,
        version: &str,
        name: &str,
        options: &[CommandArg],
        focused: &str,
    ) -> Result<Vec<CommandChoice>, super::super::Refusal> {
        if builtin_name(command_id).is_some() {
            return Ok(Vec::new());
        }
        let session_id = self.session_id()?;
        let nonce = snowflake_nonce();
        let (tx, rx) = mpsc::channel();
        self.state.lock().unwrap().autocomplete.insert(nonce.clone(), tx);
        let body = interaction_body(4, channel, self.place_of(channel), &session_id, &nonce, application_id, command_id, version, name, CommandKind::ChatInput, options, Some(focused), None, &[]);
        if let Err(err) = self.rest.call_once_with("POST", "/interactions", Some(body), &[]) {
            self.state.lock().unwrap().autocomplete.remove(&nonce);
            return Err(refusal(err));
        }
        let choices = rx.recv_timeout(AUTOCOMPLETE_WAIT).unwrap_or_default();
        self.state.lock().unwrap().autocomplete.remove(&nonce);
        Ok(choices)
    }

    /// Gateway `APPLICATION_COMMAND_AUTOCOMPLETE_RESPONSE`.
    pub(super) fn autocomplete_response(&self, data: &str) {
        let Ok(event) = serde_json::from_str::<AutocompleteEvent>(data) else {
            return;
        };
        let Some(nonce) = event.nonce.filter(|nonce| !nonce.is_empty()) else {
            return;
        };
        let choices = event.choices.iter().filter_map(choice_of).take(COMMAND_CHOICES_LIMIT).collect();
        if let Some(tx) = self.state.lock().unwrap().autocomplete.remove(&nonce) {
            let _ = tx.send(choices);
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn run_command(
        &self,
        channel: &str,
        command_id: &str,
        application_id: &str,
        version: &str,
        kind: CommandKind,
        name: &str,
        arguments: &[CommandArg],
        target_id: Option<&str>,
        files: &[OutgoingFile],
    ) -> Result<(), super::super::Refusal> {
        if let Some(builtin) = builtin_name(command_id) {
            return self.run_builtin(channel, builtin, arguments);
        }
        if application_id.is_empty() {
            return Err(super::super::Refusal::Refused("That command is missing its application.".to_owned()));
        }
        if matches!(kind, CommandKind::User | CommandKind::Message) && target_id.unwrap_or("").is_empty() {
            return Err(super::super::Refusal::Refused("Choose a message or a person first.".to_owned()));
        }
        let attachments = self.stage_files(channel, arguments, files)?;
        let session_id = self.session_id()?;
        let nonce = snowflake_nonce();
        let body = interaction_body(2, channel, self.place_of(channel), &session_id, &nonce, application_id, command_id, version, name, kind, arguments, None, target_id, &attachments);
        self.remember_interaction(
            nonce.clone(),
            super::interactions::PendingInteraction {
                conversation_id: channel.to_owned(),
                message_id: String::new(),
                action_id: name.to_owned(),
            },
        );
        if let Err(err) = self.rest.call_once_with("POST", "/interactions", Some(body), &[]) {
            self.forget_interaction(&nonce);
            return Err(refusal(err));
        }
        Ok(())
    }

    fn place_of(&self, channel: &str) -> Option<String> {
        self.ctx.known(channel).ok().flatten().as_ref().and_then(|conversation| self.guild_of(conversation))
    }

    fn stage_files(&self, channel: &str, arguments: &[CommandArg], files: &[OutgoingFile]) -> Result<Vec<Value>, super::super::Refusal> {
        let needed = arguments.iter().filter(|arg| arg.kind == CommandOptionKind::Attachment).count();
        if needed > files.len() {
            return Err(super::super::Refusal::Refused("Attach a file for this command.".to_owned()));
        }
        if files.is_empty() {
            return Ok(Vec::new());
        }
        let mut upload = Upload::new(&self.ctx, channel, &snowflake_nonce(), files).map_err(super::super::Refusal::Refused)?;
        self.upload(channel, &mut upload).map_err(super::super::Refusal::Refused)
    }

    fn run_builtin(&self, channel: &str, name: &str, arguments: &[CommandArg]) -> Result<(), super::super::Refusal> {
        let text = arguments.first().map(|arg| arg.value.as_str()).unwrap_or("");
        match name {
            "shrug" | "tableflip" | "unflip" | "me" | "spoiler" => self.say(channel, &rewrite(name, text).unwrap_or_default()),
            "nick" => self.set_nick(channel, text),
            "thread" => self.open_thread(channel, text),
            _ => Err(super::super::Refusal::Refused("That command is not available.".to_owned())),
        }
    }

    fn say(&self, channel: &str, text: &str) -> Result<(), super::super::Refusal> {
        let client_id = snowflake_nonce();
        let outcome = self.send(channel, &client_id, text, None);
        self.ctx.sent(channel, &client_id, outcome.clone());
        outcome.map(|_| ()).map_err(super::super::Refusal::Refused)
    }

    fn set_nick(&self, channel: &str, nick: &str) -> Result<(), super::super::Refusal> {
        let guild = self.place_of(channel).ok_or_else(|| super::super::Refusal::Refused("Nicknames are set in a server.".to_owned()))?;
        let nick = if nick.is_empty() { Value::Null } else { json!(nick) };
        self.call("PATCH", &format!("/guilds/{guild}/members/@me"), Some(json!({"nick": nick}))).map_err(refusal)?;
        Ok(())
    }

    fn open_thread(&self, channel: &str, name: &str) -> Result<(), super::super::Refusal> {
        let parent = self
            .ctx
            .known(channel)
            .map_err(|err| super::super::Refusal::Refused(err.to_string()))?
            .ok_or_else(|| super::super::Refusal::Refused("This conversation is gone.".to_owned()))?;
        if !matches!(parent.kind, ConversationKind::Channel | ConversationKind::Announcement | ConversationKind::Forum) {
            return Err(super::super::Refusal::Refused("A thread starts in a channel.".to_owned()));
        }
        let name = super::thread_name(name);
        let created = self.call("POST", &format!("/channels/{channel}/threads"), Some(json!({"name": name}))).map_err(refusal)?;
        let thread: super::model::Channel = serde_json::from_str(&created).map_err(|err| super::super::Refusal::Refused(format!("Discord's answer was unreadable: {err}")))?;
        let title = thread.name.clone().filter(|title| !title.trim().is_empty()).unwrap_or(name);
        self.record_thread(&parent, &thread, None, &title)
    }
}

fn sources(channel: &str, guild: Option<&str>) -> Vec<(String, String, bool)> {
    match guild {
        Some(guild) => vec![
            (format!("guild:{guild}"), format!("/guilds/{guild}/application-command-index"), true),
            ("user".to_owned(), "/users/@me/application-command-index".to_owned(), false),
        ],
        None => vec![(format!("dm:{channel}"), format!("/channels/{channel}/application-command-index"), true)],
    }
}

fn builtins() -> Vec<AppCommand> {
    vec![
        builtin("shrug", "Append a shrug", "text"),
        builtin("tableflip", "Flip a table", "text"),
        builtin("unflip", "Put the table back", "text"),
        builtin("me", "Italic action", "text"),
        builtin("spoiler", "Hide text", "text"),
        builtin("nick", "Set your nickname here", "nick"),
        builtin("thread", "Start a thread", "name"),
    ]
}

fn builtin(name: &str, description: &str, option: &str) -> AppCommand {
    AppCommand {
        id: format!("{BUILTIN_PREFIX}{name}"),
        name: name.to_owned(),
        description: description.to_owned(),
        kind: CommandKind::ChatInput,
        built_in: true,
        options: vec![CommandOption {
            name: option.to_owned(),
            kind: CommandOptionKind::String,
            ..CommandOption::default()
        }],
        ..AppCommand::default()
    }
}

fn builtin_name(command_id: &str) -> Option<&str> {
    match command_id.strip_prefix(BUILTIN_PREFIX)? {
        name @ ("shrug" | "tableflip" | "unflip" | "me" | "spoiler" | "nick" | "thread") => Some(name),
        _ => None,
    }
}

fn rewrite(name: &str, text: &str) -> Option<String> {
    let tail = match name {
        "shrug" => SHRUG,
        "tableflip" => TABLEFLIP,
        "unflip" => UNFLIP,
        "me" => return Some(format!("*{text}*")),
        "spoiler" => return Some(format!("||{text}||")),
        _ => return None,
    };
    Some(if text.is_empty() { tail.to_owned() } else { format!("{text} {tail}") })
}

fn parse_index(body: &str) -> Result<Vec<AppCommand>, String> {
    let index: Index = serde_json::from_str(body).map_err(|err| format!("Discord's answer was unreadable: {err}"))?;
    let mut names = std::collections::HashMap::new();
    for app in index.applications {
        if !app.id.is_empty() {
            names.insert(app.id, app.name);
        }
    }
    let mut out = Vec::new();
    for command in index.application_commands {
        let Some(kind) = command_kind(command.kind) else {
            continue;
        };
        if command.id.is_empty() || command.name.is_empty() {
            continue;
        }
        out.push(AppCommand {
            id: command.id,
            application_id: command.application_id.clone(),
            version: command.version,
            kind,
            name: command.name,
            description: command.description,
            app_name: names.get(&command.application_id).cloned().unwrap_or_default(),
            options: map_options(&command.options),
            built_in: false,
        });
    }
    Ok(out)
}

fn map_options(raw: &[IndexOption]) -> Vec<CommandOption> {
    raw.iter().filter_map(map_option).take(COMMAND_OPTIONS_LIMIT).collect()
}

fn map_option(raw: &IndexOption) -> Option<CommandOption> {
    if raw.name.is_empty() {
        return None;
    }
    Some(CommandOption {
        name: raw.name.clone(),
        description: raw.description.clone(),
        kind: option_kind(raw.kind)?,
        required: raw.required,
        autocomplete: raw.autocomplete,
        choices: raw.choices.iter().filter_map(choice_of).take(COMMAND_CHOICES_LIMIT).collect(),
        options: map_options(&raw.options),
    })
}

fn choice_of(raw: &IndexChoice) -> Option<CommandChoice> {
    if raw.name.is_empty() {
        return None;
    }
    Some(CommandChoice { name: raw.name.clone(), value: json_text(&raw.value)? })
}

fn json_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        _ => None,
    }
}

fn command_kind(type_num: u8) -> Option<CommandKind> {
    match type_num {
        1 => Some(CommandKind::ChatInput),
        2 => Some(CommandKind::User),
        3 => Some(CommandKind::Message),
        _ => None,
    }
}

fn option_kind(type_num: u8) -> Option<CommandOptionKind> {
    Some(match type_num {
        1 => CommandOptionKind::SubCommand,
        2 => CommandOptionKind::SubCommandGroup,
        3 => CommandOptionKind::String,
        4 => CommandOptionKind::Integer,
        5 => CommandOptionKind::Boolean,
        6 => CommandOptionKind::User,
        7 => CommandOptionKind::Channel,
        8 => CommandOptionKind::Role,
        9 => CommandOptionKind::Mentionable,
        10 => CommandOptionKind::Number,
        11 => CommandOptionKind::Attachment,
        _ => return None,
    })
}

fn option_type(kind: CommandOptionKind) -> u8 {
    match kind {
        CommandOptionKind::SubCommand => 1,
        CommandOptionKind::SubCommandGroup => 2,
        CommandOptionKind::String => 3,
        CommandOptionKind::Integer => 4,
        CommandOptionKind::Boolean => 5,
        CommandOptionKind::User => 6,
        CommandOptionKind::Channel => 7,
        CommandOptionKind::Role => 8,
        CommandOptionKind::Mentionable => 9,
        CommandOptionKind::Number => 10,
        CommandOptionKind::Attachment => 11,
    }
}

fn kind_type(kind: CommandKind) -> u8 {
    match kind {
        CommandKind::ChatInput => 1,
        CommandKind::User => 2,
        CommandKind::Message => 3,
    }
}

fn arg_item(arg: &CommandArg, focused: Option<&str>, attachments: &[Value], file_at: &mut usize) -> Value {
    let focusing = focused == Some(arg.name.as_str());
    let value = if focusing {
        json!(arg.value)
    } else {
        match arg.kind {
            CommandOptionKind::Integer => arg.value.parse::<i64>().map(|number| json!(number)).unwrap_or_else(|_| json!(arg.value)),
            CommandOptionKind::Number => arg.value.parse::<f64>().map(|number| json!(number)).unwrap_or_else(|_| json!(arg.value)),
            CommandOptionKind::Boolean => json!(matches!(arg.value.to_ascii_lowercase().as_str(), "true" | "1" | "yes")),
            CommandOptionKind::Attachment => {
                let id = attachments.get(*file_at).and_then(|file| file["id"].as_str()).unwrap_or("0");
                *file_at += 1;
                json!(id)
            }
            _ => json!(arg.value),
        }
    };
    let mut item = json!({"type": option_type(arg.kind), "name": arg.name, "value": value});
    if focusing {
        item["focused"] = json!(true);
    }
    item
}

/// A subcommand or group, with every following argument nested inside it.
/// A group holds its subcommand; that subcommand holds the values.
fn take_branch(
    arguments: &[CommandArg],
    index: usize,
    focused: Option<&str>,
    attachments: &[Value],
    file_at: &mut usize,
) -> (Value, usize) {
    let arg = &arguments[index];
    let mut node = json!({"type": option_type(arg.kind), "name": arg.name});
    let mut next = index + 1;
    let mut children = Vec::new();
    if arg.kind == CommandOptionKind::SubCommandGroup
        && arguments.get(next).is_some_and(|child| child.kind == CommandOptionKind::SubCommand)
    {
        let (sub, at) = take_branch(arguments, next, focused, attachments, file_at);
        children.push(sub);
        next = at;
    } else {
        while arguments.get(next).is_some_and(|child| {
            !matches!(child.kind, CommandOptionKind::SubCommand | CommandOptionKind::SubCommandGroup)
        }) {
            children.push(arg_item(&arguments[next], focused, attachments, file_at));
            next += 1;
        }
    }
    if !children.is_empty() {
        node["options"] = json!(children);
    }
    (node, next)
}

#[allow(clippy::too_many_arguments)]
fn interaction_body(
    interaction_type: u8,
    channel: &str,
    guild: Option<String>,
    session_id: &str,
    nonce: &str,
    application_id: &str,
    command_id: &str,
    version: &str,
    name: &str,
    kind: CommandKind,
    arguments: &[CommandArg],
    focused: Option<&str>,
    target_id: Option<&str>,
    attachments: &[Value],
) -> Value {
    let mut file_at = 0usize;
    let mut options = Vec::new();
    let mut index = 0;
    while index < arguments.len() {
        if matches!(arguments[index].kind, CommandOptionKind::SubCommand | CommandOptionKind::SubCommandGroup) {
            let (node, next) = take_branch(arguments, index, focused, attachments, &mut file_at);
            options.push(node);
            index = next;
        } else {
            options.push(arg_item(&arguments[index], focused, attachments, &mut file_at));
            index += 1;
        }
    }
    let mut data = json!({
        "id": command_id,
        "type": kind_type(kind),
        "name": name,
        "version": version,
    });
    if !options.is_empty() {
        data["options"] = json!(options);
    }
    if let Some(target) = target_id.filter(|id| !id.is_empty()) {
        data["target_id"] = json!(target);
    }
    if !attachments.is_empty() {
        data["attachments"] = json!(attachments);
    }
    let mut body = json!({
        "type": interaction_type,
        "application_id": application_id,
        "channel_id": channel,
        "session_id": session_id,
        "nonce": nonce,
        "data": data,
    });
    if let Some(guild) = guild {
        body["guild_id"] = json!(guild);
    }
    body
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_keeps_every_kind_and_names_the_app() {
        let raw = r#"{
            "applications": [{"id": "555", "name": "Lightbulb"}],
            "application_commands": [
                {"id": "9001", "application_id": "555", "version": "3", "type": 1, "name": "deploy", "description": "Ship it",
                 "options": [{"type": 3, "name": "env", "description": "Where", "required": true, "autocomplete": true,
                              "choices": [{"name": "Staging", "value": "staging"}, {"name": "Seven", "value": 7}]}]},
                {"id": "9002", "application_id": "555", "version": "1", "type": 3, "name": "Pin"},
                {"id": "9003", "application_id": "555", "version": "1", "type": 2, "name": "Profile"}
            ],
            "version": "42"
        }"#;
        let cmds = parse_index(raw).unwrap();
        assert_eq!(cmds.len(), 3);
        assert_eq!(cmds[0].app_name, "Lightbulb");
        assert_eq!(cmds[0].kind, CommandKind::ChatInput);
        assert!(cmds[0].options[0].autocomplete && cmds[0].options[0].required);
        assert_eq!(cmds[0].options[0].choices[1].value, "7");
        assert_eq!(cmds[1].kind, CommandKind::Message);
        assert_eq!(cmds[2].kind, CommandKind::User);
    }

    #[test]
    fn subcommand_arguments_nest_under_the_definition_name() {
        let args = vec![
            CommandArg { name: "user".into(), value: String::new(), kind: CommandOptionKind::SubCommandGroup },
            CommandArg { name: "grant".into(), value: String::new(), kind: CommandOptionKind::SubCommand },
            CommandArg { name: "perm".into(), value: "admin".into(), kind: CommandOptionKind::String },
        ];
        let body = interaction_body(
            2,
            "4101",
            None,
            "session",
            "nonce",
            "app",
            "cmd",
            "1",
            "mod",
            CommandKind::ChatInput,
            &args,
            None,
            None,
            &[],
        );
        assert_eq!(
            body["data"]["options"],
            json!([{
                "type": 2,
                "name": "user",
                "options": [{
                    "type": 1,
                    "name": "grant",
                    "options": [{"type": 3, "name": "perm", "value": "admin"}]
                }]
            }])
        );
    }

    #[test]
    fn builtins_rewrite_the_documented_way() {
        assert_eq!(rewrite("shrug", "well").as_deref(), Some("well ¯\\_(ツ)_/¯"));
        assert_eq!(rewrite("shrug", "").as_deref(), Some("¯\\_(ツ)_/¯"));
        assert_eq!(rewrite("tableflip", "").as_deref(), Some("(╯°□°）╯︵ ┻━┻"));
        assert_eq!(rewrite("unflip", "ok").as_deref(), Some("ok ┬─┬ ノ( ゜-゜ノ)"));
        assert_eq!(rewrite("me", "waves").as_deref(), Some("*waves*"));
        assert_eq!(rewrite("spoiler", "secret").as_deref(), Some("||secret||"));
        assert_eq!(rewrite("nick", "x"), None);
    }
}
