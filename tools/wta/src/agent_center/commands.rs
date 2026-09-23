// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub mod preparation;
#[path = "commands\\request_file.rs"]
mod request_file;

#[derive(Clone, Copy)]
pub struct Definition {
    pub name: &'static str,
    pub method: Option<&'static str>,
    pub arguments: &'static str,
    pub mutation: bool,
    pub confirmation: bool,
}

macro_rules! command {
    ($name:literal, $method:expr, $args:literal, $mutation:literal, $confirm:literal) => {
        Definition {
            name: $name,
            method: $method,
            arguments: $args,
            mutation: $mutation,
            confirmation: $confirm,
        }
    };
}

pub static REGISTRY: &[Definition] = &[
    command!("home", None, "", false, false),
    command!("help", None, "[command]", false, false),
    command!("history", None, "", false, false),
    command!("work new", Some("conversation.submit"), "<goal> [--project <id>]", true, false),
    command!("work list", Some("work.list"), "[--after <id>]", false, false),
    command!("work use", None, "<id>", false, false),
    command!("work show", Some("work.get"), "[id] [--work <id>]", false, false),
    command!("work events", Some("events.subscribe"), "[id] [--after <cursor>]", false, false),
    command!("work start", Some("work.start"), "[--work <id>] --version <n> --spec-revision <n> --policy-revision <n> --grant <id> --confirm", true, true),
    command!("work pause", Some("work.control"), "[--work <id>] [--version <n>]", true, false),
    command!("work resume", Some("work.control"), "[--work <id>] [--version <n>]", true, false),
    command!("work cancel", Some("work.control"), "[--work <id>] [--version <n>] --confirm", true, true),
    command!("work revise", Some("work.propose_change"), "<request> [--work <id>] [--project <id>] [--conversation <id>] [--message-id <id>] | --input-json <file|-> | --params-json <json>", true, false),
    command!("work apply", Some("work.apply_change"), "<proposal-id> [--work <id>] [--work-version <n>] [--proposal-version <n>] [--confirm] | --input-json <file|-> | --params-json <json>", true, true),
    command!("plan show", Some("work.get"), "[--work <id>]", false, false),
    command!("plan revise", Some("conversation.submit"), "<requested-changes> [--work <id>] [--project <id>] [--conversation <id>]", true, false),
    command!("task list", Some("task.list"), "[--work <id>]", false, false),
    command!("task show", Some("task.get"), "<id>", false, false),
    command!("result show", Some("result.get"), "<id>", false, false),
    command!("artifact show", Some("artifact.get"), "<id>", false, false),
    command!("evidence show", Some("artifact.get"), "<id>", false, false),
    command!("decision list", Some("decision.list"), "[--work <id>]", false, false),
    command!("decision show", Some("decision.get"), "<id>", false, false),
    command!("decision answer", Some("decision.answer"), "<id> --value <json> [--version <n>]", true, false),
    command!("intake answer", Some("conversation.answer_input"), "<id> --value <json> --version <n>", true, false),
    command!("intake cancel", Some("conversation.answer_input"), "<id> --version <n>", true, false),
    command!("inbox", Some("inbox.list"), "[--work <id>]", false, false),
    command!("review list", Some("delivery.list"), "[--work <id>]", false, false),
    command!("review show", Some("delivery.get"), "<id>", false, false),
    command!("review accept", Some("delivery.accept"), "<id> [--work <id>] --version <n> --work-version <n> --confirm", true, true),
    command!("review revise", Some("delivery.request_changes"), "<id> --findings <json> [--preserve <json>] [--advance] --version <n> --work-version <n> --confirm", true, true),
    command!("workspace show", Some("workspace.inspect"), "[--work <id>]", false, false),
    command!("workspace get", Some("workspace.get"), "<id>", false, false),
    command!("workspace takeover", Some("workspace.takeover"), "[--work <id>] [--version <n>] [--work-version <n>] [--input-json <file|-> | --params-json <json>] --confirm", true, true),
    command!("workspace handback", Some("workspace.handback"), "[--work <id>] --summary <text> --resume-affected <true|false> [--version <n>] [--work-version <n>] | --input-json <file|-> | --params-json <json> --confirm", true, true),
    command!("project list", Some("project.list"), "", false, false),
    command!("project show", Some("project.get"), "[id] [--project <id>]", false, false),
    command!("project configure", Some("project.configure"), "--input-json <file|-> | --params-json <json> --confirm", true, true),
    command!("project use", None, "<id>", false, false),
    command!("grant preview", Some("grant.preview"), "[--work <id>] --version <n> --spec-revision <n> --policy-revision <n>", true, false),
    command!("operation show", Some("operation.get"), "<id>", false, false),
];

const UNSUPPORTED: &[&str] = &[
    "work priority",
    "work reopen",
    "work archive",
    "plan dependency add",
    "plan dependency remove",
    "agent list",
    "agent assign",
    "agent auto",
    "run list",
    "run show",
    "run stop",
    "run retry",
    "inbox seen",
    "inbox snooze",
    "artifact list",
    "artifact diff",
    "delivery show",
    "delivery publish",
    "shell list",
    "shell show",
    "shell hide",
    "shell focus",
    "shell open",
    "shell attach",
    "shell close",
    "workspace repair",
    "workspace cleanup",
    "context show",
    "context add",
    "context remove",
    "usage",
    "capacity",
    "capacity set",
    "grant show",
    "grant revise",
    "operation list",
    "operation repair",
];

#[derive(Clone, Default, Debug)]
pub struct CommandContext {
    pub global_conversation: bool,
    pub work_id: Option<String>,
    pub project_id: Option<String>,
    pub conversation_id: String,
    pub console_session_id: String,
    pub context_version: u64,
    pub versions: BTreeMap<(String, String), u64>,
}

impl CommandContext {
    fn change_conversation(&mut self, conversation_id: String) {
        if self.conversation_id != conversation_id {
            self.conversation_id = conversation_id;
            self.console_session_id = uuid::Uuid::new_v4().to_string();
        }
    }
}

#[derive(Clone, Debug)]
pub struct Operation {
    pub method: String,
    pub params: Value,
    pub if_match: Vec<Value>,
    pub mutation: bool,
    pub confirmation: bool,
    pub command_id: String,
}

impl Operation {
    pub fn read(method: &str, params: Value) -> Self {
        Self {
            method: method.into(),
            params,
            if_match: vec![],
            mutation: false,
            confirmation: false,
            command_id: uuid::Uuid::new_v4().to_string(),
        }
    }

    pub fn envelope(&self) -> Value {
        let mut request = json!({
            "type": "request",
            "requestId": uuid::Uuid::new_v4().to_string(),
            "method": self.method,
            "ifMatch": self.if_match,
            "params": self.params,
        });
        if self.mutation {
            request["commandId"] = json!(self.command_id);
        }
        request
    }
}

#[derive(Clone, Debug)]
pub enum Action {
    Operation(Operation),
    Home,
    Help(String),
    SelectWork(String),
    SelectProject(String),
    PrepareStart(String),
    PrepareApply(preparation::ApplyTarget),
    PrepareTransfer(preparation::TransferTarget),
    PrepareProposal(preparation::ProposalIntake),
    Unsupported(String),
}

#[derive(Debug, PartialEq)]
pub enum Input {
    Text(String),
    Command(Vec<String>),
}

/// Single quotes are literal; double quotes support escaped quote/backslash.
/// Unquoted backslashes remain literal so Windows and UNC paths survive.
pub fn tokenize(text: &str) -> Result<Vec<String>> {
    let mut words = vec![];
    let mut word = String::new();
    let mut quote = None;
    let mut started = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\'
            && quote == Some('"')
            && chars
                .peek()
                .is_some_and(|next| *next == '\\' || Some(*next) == quote)
        {
            if let Some(next) = chars.next() {
                word.push(next);
                started = true;
            }
        } else if let Some(delimiter) = quote {
            if c == delimiter {
                quote = None;
            } else {
                word.push(c);
            }
        } else if c == '\'' || c == '"' {
            quote = Some(c);
            started = true;
        } else if c.is_whitespace() {
            if started {
                words.push(std::mem::take(&mut word));
                started = false;
            }
        } else {
            word.push(c);
            started = true;
        }
    }
    if quote.is_some() {
        bail!("{}", t!("agent_center.unclosed_quote"));
    }
    if started {
        words.push(word);
    }
    Ok(words)
}

pub fn parse_input(text: &str) -> Result<Input> {
    let trimmed = text.trim_start();
    if let Some(literal) = trimmed.strip_prefix("//") {
        Ok(Input::Text(format!("/{literal}")))
    } else if let Some(command) = trimmed.strip_prefix('/') {
        Ok(Input::Command(tokenize(command)?))
    } else {
        Ok(Input::Text(text.into()))
    }
}

pub fn completions(prefix: &str) -> Vec<String> {
    let prefix = prefix.trim_start().trim_start_matches('/');
    REGISTRY
        .iter()
        .map(|entry| entry.name)
        .chain(UNSUPPORTED.iter().copied())
        .filter(|name| name.starts_with(prefix))
        .map(|name| format!("/{name}"))
        .collect()
}

pub fn complete_with_context(input: &str, context: &CommandContext) -> Vec<String> {
    let named = completions(input);
    if !named.is_empty() {
        return named;
    }
    let text = input.trim_start().trim_start_matches('/');
    let Ok(mut tokens) = tokenize(text) else {
        return vec![];
    };
    let partial = if text.ends_with(char::is_whitespace) {
        String::new()
    } else {
        tokens.pop().unwrap_or_default()
    };
    let prefix = tokens.join(" ");
    let kind = if tokens.last().is_some_and(|token| token == "--work") {
        "Work"
    } else {
        match prefix.as_str() {
            "work use" | "work show" | "work events" => "Work",
            "work apply" => "ChangeProposal",
            "task show" => "Task",
            "decision show" | "decision answer" => "DecisionRequest",
            "review show" | "review accept" | "review revise" => "DeliveryCandidate",
            "artifact show" | "evidence show" => "Artifact",
            "project use" | "project show" => "Project",
            "operation show" => "Operation",
            "workspace get" => "Workspace",
            _ => return vec![],
        }
    };
    context
        .versions
        .keys()
        .filter(|(entity, id)| entity == kind && id.starts_with(&partial))
        .map(|(_, id)| format!("/{prefix} {id}"))
        .collect()
}

pub fn help(prefix: &str) -> Value {
    json!({
        "commands": REGISTRY.iter().filter(|d| d.name.starts_with(prefix)).map(|d| json!({
            "command": d.name, "method": d.method, "arguments": d.arguments,
            "requestArguments": d.method.map(|_| "[target] --input-json <file|-> [--json]"),
            "mutation": d.mutation, "confirmation": d.confirmation,
        })).collect::<Vec<_>>(),
        "unsupported": UNSUPPORTED.iter().filter(|name| name.starts_with(prefix)).collect::<Vec<_>>(),
        "mutationOptions": ["--command-id <uuid>"],
        "preparedCommands":{"work apply":{"target":"proposal-id","work":"selected-or-explicit",
            "reads":["work.get","project.get"],"preview":"grant.preview","cliRequires":["--work","--confirm"],
            "consoleConfirmation":true},
            "work revise":{"input":"natural-language-request","work":"selected-or-explicit",
                "reads":["work.get"],"method":"conversation.submit","declaredIntent":"ChangeProposal",
                "cliRequires":["--work"],"appliesChanges":false,"typedInputMethod":"work.propose_change"},
            "workspace takeover":{"work":"selected-or-explicit","reads":["work.get","workspace.get","workspace.inspect"],
                "cliRequires":["--work","--confirm"],"consoleConfirmation":true,"ifMatch":["Workspace"]},
            "workspace handback":{"work":"selected-or-explicit","reads":["work.get","workspace.get","workspace.inspect"],
                "cliRequires":["--work","--summary","--resume-affected","--confirm"],"consoleConfirmation":true,"ifMatch":["Workspace"],
                "resumeAffectedRequired":true,"resumesWorkHold":false}},
        "payloadOptions": {"--input-json":{"source":"file-path-or-stdin-dash","schemas":["Request","Params"]},
            "--params-json":"inline-params-json"},
        "requestContract": {
            "required":["method","ifMatch","params"],
            "mutationsRequire":["commandId"],
            "explicitArguments":"must-match-document",
            "selectedContextDefaults":false,
            "cliSubmissionConfirmsCapturedRequest":true,
            "consolePreviewsConsequentialRequests":true,
            "paramsOnlyMethods":["project.configure","workspace.takeover","workspace.handback","work.propose_change","work.apply_change"],
        },
        "payloadContracts": {
            "project.configure": {"required":["name","root","coordinatorCapabilityId","workerCapabilityId","checkCapabilityId","limits"],
                "limitFields":["concurrency","executionAttempts","evaluationAttempts","coordinationTurns","contextRounds","executionSeconds","coordinationSeconds"],
                "ifMatch":[]},
            "workspace.takeover":{"required":["workspaceId"],"ifMatch":["Workspace"]},
            "workspace.handback":{"required":["workspaceId","summary","resumeAffected"],"ifMatch":["Workspace"]},
            "work.propose_change":{"required":["workId","replacementSpec","affectedTaskIds","reason"],"ifMatch":["Work"]},
            "work.apply_change":{"required":["proposalId","grantProposalId"],"ifMatch":["Work","ChangeProposal"]},
        },
    })
}

pub fn unsupported_failure(command: &str) -> (&'static str, String) {
    if command.split_whitespace().next() == Some("shell") {
        (
            "CAPABILITY_UNAVAILABLE",
            t!("agent_center.host_unavailable", command = command).into_owned(),
        )
    } else {
        (
            "METHOD_UNSUPPORTED",
            t!("agent_center.unsupported", command = command).into_owned(),
        )
    }
}

struct Arguments {
    positional: Vec<String>,
    options: BTreeMap<String, String>,
}

impl Arguments {
    fn parse(args: &[String]) -> Result<Self> {
        let mut result = Self {
            positional: vec![],
            options: BTreeMap::new(),
        };
        let mut iter = args.iter();
        let mut literal = false;
        while let Some(arg) = iter.next() {
            if arg == "--" && !literal {
                literal = true;
                continue;
            }
            if !literal && arg.starts_with("--") {
                let key = arg.trim_start_matches("--");
                let flag = matches!(key, "confirm" | "advance" | "json" | "jsonl");
                let value = if flag {
                    "true".into()
                } else {
                    iter.next().cloned().with_context(|| {
                        t!("agent_center.missing_argument", argument = arg).into_owned()
                    })?
                };
                if result.options.insert(key.into(), value).is_some() {
                    bail!("{}", t!("agent_center.duplicate_argument", argument = arg));
                }
            } else {
                result.positional.push(arg.clone());
            }
        }
        Ok(result)
    }

    fn take(&mut self, name: &str) -> Option<String> {
        self.options.remove(name)
    }
    fn required(&mut self, name: &str) -> Result<String> {
        self.take(name).with_context(|| {
            t!(
                "agent_center.missing_argument",
                argument = format!("--{name}")
            )
            .into_owned()
        })
    }
    fn id(&mut self) -> Result<String> {
        if self.positional.is_empty() {
            bail!("{}", t!("agent_center.target_required"));
        }
        Ok(self.positional.remove(0))
    }
    fn number(&mut self, name: &str) -> Result<u64> {
        let number = self
            .required(name)?
            .parse::<u64>()
            .with_context(|| t!("agent_center.invalid_version").into_owned())?;
        if number == 0 {
            bail!("{}", t!("agent_center.invalid_version"));
        }
        Ok(number)
    }
    fn guard(
        &mut self,
        context: &CommandContext,
        kind: &str,
        id: &str,
        option: &str,
    ) -> Result<Value> {
        let version = if self.options.contains_key(option) {
            self.number(option)?
        } else {
            *context
                .versions
                .get(&(kind.into(), id.into()))
                .with_context(|| {
                    t!("agent_center.version_required", kind = kind, id = id).into_owned()
                })?
        };
        Ok(json!({"kind": kind, "id": id, "version": version}))
    }
    fn work(&mut self, context: &CommandContext) -> Result<String> {
        self.take("work")
            .or_else(|| context.work_id.clone())
            .with_context(|| t!("agent_center.work_required").into_owned())
    }
    fn params_json(&mut self) -> Result<Value> {
        let value: Value = serde_json::from_str(&self.required("params-json")?)?;
        if !value.is_object() {
            bail!("{}", t!("agent_center.invalid_request"));
        }
        Ok(value)
    }
    fn finish(mut self) -> Result<()> {
        self.take("json");
        self.take("jsonl");
        if !self.positional.is_empty() || !self.options.is_empty() {
            bail!(
                "{}",
                t!(
                    "agent_center.unexpected_arguments",
                    arguments = format!("{:?} {:?}", self.positional, self.options.keys())
                )
            );
        }
        Ok(())
    }
}

pub fn conversation(text: String, context: &CommandContext, new_work: bool) -> Result<Operation> {
    if text.trim().is_empty() {
        bail!("{}", t!("agent_center.empty_message"));
    }
    if !context.global_conversation && context.project_id.is_none() {
        bail!("{}", t!("agent_center.project_required"));
    }
    let mut captured = json!({
        "consoleSessionId": context.console_session_id,
        "contextVersion": context.context_version.max(1),
    });
    if context.global_conversation {
        captured["scope"] = json!("Global");
    }
    if let Some(project) = &context.project_id {
        captured["projectId"] = json!(project);
    }
    if let Some(work) = &context.work_id {
        captured["selectedWorkId"] = json!(work);
    }
    let mut params = json!({
        "conversationId": context.conversation_id,
        "clientMessageId": uuid::Uuid::new_v4().to_string(),
        "text": text, "attachments": [], "context": captured,
    });
    if new_work {
        params["declaredIntent"] = json!("NewWork");
    }
    Ok(Operation {
        method: "conversation.submit".into(),
        params,
        if_match: vec![],
        mutation: true,
        confirmation: false,
        command_id: uuid::Uuid::new_v4().to_string(),
    })
}

pub fn compile(args: &[String], context: &CommandContext, interactive: bool) -> Result<Action> {
    if args.last().is_some_and(|arg| arg == "--help") && !args.iter().any(|arg| arg == "--") {
        return Ok(Action::Help(args[..args.len() - 1].join(" ")));
    }
    if args.len() == 2 && args[1] == "help" {
        return Ok(Action::Help(args[0].clone()));
    }
    let definition = REGISTRY
        .iter()
        .filter(|definition| {
            let words: Vec<_> = definition.name.split_whitespace().collect();
            args.len() >= words.len() && args.iter().zip(words).all(|(a, b)| a == b)
        })
        .max_by_key(|definition| definition.name.len());
    let Some(definition) = definition else {
        let name = args.join(" ");
        if REGISTRY
            .iter()
            .any(|entry| entry.name.starts_with(&format!("{name} ")))
        {
            return Ok(Action::Help(name));
        }
        return Ok(Action::Unsupported(name));
    };
    let count = definition.name.split_whitespace().count();
    let mut a = Arguments::parse(&args[count..])?;
    let confirmed = a.take("confirm").is_some();
    let command_id = a
        .take("command-id")
        .map(|id| {
            uuid::Uuid::parse_str(&id)
                .with_context(|| t!("agent_center.invalid_command_id").into_owned())
                .map(|_| id)
        })
        .transpose()?;
    if let Some(request) = a.take("request-json") {
        return request_file::compile(definition, a, &request, command_id, confirmed, interactive)
            .map(Action::Operation);
    }
    let mut operation = Operation {
        method: definition.method.unwrap_or_default().into(),
        params: json!({}),
        if_match: vec![],
        mutation: definition.mutation,
        confirmation: definition.confirmation && !confirmed,
        command_id: uuid::Uuid::new_v4().to_string(),
    };
    let mut action = match definition.name {
        "home" => Action::Home,
        "history" => Action::Unsupported("history".into()),
        "help" => Action::Help(std::mem::take(&mut a.positional).join(" ")),
        "work use" => Action::SelectWork(a.id()?),
        "project use" => Action::SelectProject(a.id()?),
        "work new" => {
            let mut context = context.clone();
            let previous_project = context.project_id.clone();
            context.project_id = a.take("project").or(context.project_id);
            let conversation_id = a.take("conversation");
            if context.global_conversation
                && conversation_id
                    .as_ref()
                    .is_some_and(|id| id != &context.conversation_id)
            {
                bail!("{}", t!("agent_center.invalid_request"));
            }
            let message_id = a.take("message-id");
            if interactive && !context.global_conversation {
                let target = conversation_id.clone().unwrap_or_else(|| {
                    if context.work_id.is_some() || context.project_id != previous_project {
                        uuid::Uuid::new_v4().to_string()
                    } else {
                        context.conversation_id.clone()
                    }
                });
                context.change_conversation(target);
            }
            // NewWork intake must not borrow the selected work's context.
            context.work_id = None;
            let goal = a.id()?;
            operation = conversation(goal, &context, true)?;
            if let Some(id) = &command_id {
                operation.command_id = id.clone();
            }
            if !interactive {
                operation.params["conversationId"] =
                    json!(conversation_id.unwrap_or_else(|| operation.command_id.clone()));
                operation.params["clientMessageId"] =
                    json!(message_id.unwrap_or_else(|| operation.command_id.clone()));
                operation.params["context"]["consoleSessionId"] = json!(operation.command_id);
            } else {
                if let Some(id) = conversation_id {
                    operation.params["conversationId"] = json!(id);
                }
                if let Some(id) = message_id {
                    operation.params["clientMessageId"] = json!(id);
                }
            }
            Action::Operation(operation)
        }
        "plan revise" => {
            let mut captured = context.clone();
            captured.work_id = Some(a.work(context)?);
            captured.project_id = a.take("project").or(captured.project_id);
            let conversation_id = a.take("conversation");
            if captured.global_conversation
                && conversation_id
                    .as_ref()
                    .is_some_and(|id| id != &captured.conversation_id)
            {
                bail!("{}", t!("agent_center.invalid_request"));
            }
            let message_id = a.take("message-id");
            if interactive && !captured.global_conversation {
                let target = conversation_id.clone().unwrap_or_else(|| {
                    if captured.work_id != context.work_id
                        || captured.project_id != context.project_id
                    {
                        uuid::Uuid::new_v4().to_string()
                    } else {
                        captured.conversation_id.clone()
                    }
                });
                captured.change_conversation(target);
            }
            operation = conversation(a.id()?, &captured, false)?;
            operation.params["declaredIntent"] = json!("WorkDiscussion");
            if let Some(id) = &command_id {
                operation.command_id = id.clone();
            }
            if !interactive {
                operation.params["conversationId"] =
                    json!(conversation_id.unwrap_or_else(|| operation.command_id.clone()));
                operation.params["clientMessageId"] =
                    json!(message_id.unwrap_or_else(|| operation.command_id.clone()));
                operation.params["context"]["consoleSessionId"] = json!(operation.command_id);
            } else {
                if let Some(id) = conversation_id {
                    operation.params["conversationId"] = json!(id);
                }
                if let Some(id) = message_id {
                    operation.params["clientMessageId"] = json!(id);
                }
            }
            Action::Operation(operation)
        }
        "work revise" if !a.options.contains_key("params-json") => {
            let work_id = if interactive {
                a.work(context)?
            } else {
                a.required("work")?
            };
            let text = std::mem::take(&mut a.positional).join(" ");
            if text.trim().is_empty() {
                bail!("{}", t!("agent_center.empty_message"));
            }
            let mut captured = context.clone();
            let same_work = captured.work_id.as_deref() == Some(work_id.as_str());
            captured.project_id = a.take("project").or_else(|| {
                if same_work {
                    context.project_id.clone()
                } else {
                    None
                }
            });
            captured.work_id = Some(work_id);
            let id = command_id
                .clone()
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
            let conversation_id = a.take("conversation");
            if captured.global_conversation
                && conversation_id
                    .as_ref()
                    .is_some_and(|id| id != &captured.conversation_id)
            {
                bail!("{}", t!("agent_center.invalid_request"));
            }
            let message_id = a.take("message-id");
            if interactive && !captured.global_conversation {
                if let Some(conversation_id) = conversation_id {
                    captured.change_conversation(conversation_id);
                } else if !same_work || captured.project_id != context.project_id {
                    captured.change_conversation(uuid::Uuid::new_v4().to_string());
                }
            } else if !interactive {
                captured.console_session_id = preparation::intake_identity(&id, "console");
                captured.conversation_id = conversation_id
                    .unwrap_or_else(|| preparation::intake_identity(&id, "conversation"));
            }
            let client_message_id = message_id.unwrap_or_else(|| {
                if interactive {
                    uuid::Uuid::new_v4().to_string()
                } else {
                    preparation::intake_identity(&id, "message")
                }
            });
            Action::PrepareProposal(preparation::ProposalIntake {
                context: captured,
                text,
                command_id: id,
                client_message_id,
            })
        }
        "work apply" if !a.options.contains_key("params-json") => {
            if !interactive && !confirmed {
                bail!("{}", t!("agent_center.confirmation_required"));
            }
            let work_id = a.work(context)?;
            let proposal_id = a.id()?;
            let work_version = if a.options.contains_key("work-version") {
                Some(a.number("work-version")?)
            } else {
                context
                    .versions
                    .get(&("Work".into(), work_id.clone()))
                    .copied()
            };
            let proposal_version = if a.options.contains_key("proposal-version") {
                Some(a.number("proposal-version")?)
            } else {
                context
                    .versions
                    .get(&("ChangeProposal".into(), proposal_id.clone()))
                    .copied()
            };
            Action::PrepareApply(preparation::ApplyTarget {
                work_id,
                proposal_id,
                work_version,
                proposal_version,
                command_id: uuid::Uuid::new_v4().to_string(),
            })
        }
        "workspace takeover" | "workspace handback" if !a.options.contains_key("params-json") => {
            if !interactive && !confirmed {
                bail!("{}", t!("agent_center.confirmation_required"));
            }
            let work_id = a.work(context)?;
            let work_version = if a.options.contains_key("work-version") {
                Some(a.number("work-version")?)
            } else {
                context
                    .versions
                    .get(&("Work".into(), work_id.clone()))
                    .copied()
            };
            let workspace_version = if a.options.contains_key("version") {
                Some(a.number("version")?)
            } else {
                None
            };
            let handback = if definition.name == "workspace handback" {
                let summary = a.required("summary")?;
                if summary.trim().is_empty() {
                    bail!("{}", t!("agent_center.empty_message"));
                }
                let resume_affected = match a.required("resume-affected")?.as_str() {
                    "false" => false,
                    "true" => true,
                    _ => bail!("{}", t!("agent_center.invalid_request")),
                };
                Some((summary, resume_affected))
            } else {
                None
            };
            Action::PrepareTransfer(preparation::TransferTarget {
                work_id,
                work_version,
                workspace_version,
                known_versions: context.versions.clone(),
                handback,
                command_id: uuid::Uuid::new_v4().to_string(),
            })
        }
        "project configure" | "workspace takeover" | "workspace handback" | "work revise"
        | "work apply" => {
            operation.params = a.params_json()?;
            match definition.name {
                "project configure" => {}
                "workspace takeover" | "workspace handback" => {
                    let id = required_id(&operation.params, "workspaceId")?;
                    operation
                        .if_match
                        .push(a.guard(context, "Workspace", &id, "version")?);
                }
                "work revise" => {
                    let explicit = a.take("work");
                    let work = if let Some(id) = operation.params.get("workId") {
                        let id = id
                            .as_str()
                            .filter(|id| !id.is_empty())
                            .with_context(|| t!("agent_center.target_required").into_owned())?
                            .to_owned();
                        if explicit.as_ref().is_some_and(|explicit| explicit != &id) {
                            bail!("{}", t!("agent_center.invalid_request"));
                        }
                        id
                    } else {
                        explicit
                            .or_else(|| context.work_id.clone())
                            .with_context(|| t!("agent_center.work_required").into_owned())?
                    };
                    operation.params["workId"] = json!(work);
                    operation
                        .if_match
                        .push(a.guard(context, "Work", &work, "version")?);
                }
                "work apply" => {
                    let work = a.work(context)?;
                    let proposal = required_id(&operation.params, "proposalId")?;
                    required_id(&operation.params, "grantProposalId")?;
                    operation
                        .if_match
                        .push(a.guard(context, "Work", &work, "work-version")?);
                    operation.if_match.push(a.guard(
                        context,
                        "ChangeProposal",
                        &proposal,
                        "proposal-version",
                    )?);
                }
                _ => {}
            }
            Action::Operation(operation)
        }
        "work list" | "project list" => {
            operation.params = json!({"limit":100});
            if let Some(after) = a.take("after") {
                operation.params["afterId"] = json!(after);
            }
            Action::Operation(operation)
        }
        "work show" | "plan show" | "work events" => {
            let work = if a.positional.is_empty() {
                a.work(context)?
            } else {
                a.id()?
            };
            operation.params = if definition.name == "work events" {
                let mut params = json!({"scope":{"kind":"Work","id":work}});
                if let Some(after) = a.take("after") {
                    params["afterCursor"] = json!(after);
                }
                params
            } else {
                json!({"workId":work})
            };
            Action::Operation(operation)
        }
        "work start" if interactive && !a.options.contains_key("grant") => {
            Action::PrepareStart(a.work(context)?)
        }
        "work start" | "grant preview" => {
            let work = a.work(context)?;
            operation
                .if_match
                .push(a.guard(context, "Work", &work, "version")?);
            let spec = a.number("spec-revision")?;
            let policy = a.number("policy-revision")?;
            operation.params = if definition.name == "work start" {
                json!({"workId":work, "specRevision":spec, "projectPolicyRevision":policy,
                    "grantProposalId":a.required("grant")?})
            } else {
                json!({"workId":work,"specRevision":spec,"policyRevision":policy})
            };
            Action::Operation(operation)
        }
        "work pause" | "work resume" | "work cancel" => {
            let work = a.work(context)?;
            operation
                .if_match
                .push(a.guard(context, "Work", &work, "version")?);
            let action = match definition.name {
                "work pause" => "Hold",
                "work resume" => "Resume",
                _ => "Cancel",
            };
            operation.params = json!({"workId":work, "action":action});
            Action::Operation(operation)
        }
        "task list" => {
            operation.params = json!({"workId":a.work(context)?,"limit":100});
            if let Some(after) = a.take("after") {
                operation.params["afterId"] = json!(after);
            }
            Action::Operation(operation)
        }
        "inbox" | "decision list" | "review list" => {
            operation.params = json!({"limit":100});
            if let Some(after) = a.take("after") {
                operation.params["afterId"] = json!(after);
            }
            if let Some(work) = a.take("work").or_else(|| {
                (definition.name != "inbox")
                    .then(|| context.work_id.clone())
                    .flatten()
            }) {
                operation.params["workId"] = json!(work);
            }
            Action::Operation(operation)
        }
        "task show" | "result show" | "artifact show" | "evidence show" | "decision show"
        | "review show" | "operation show" | "workspace get" => {
            let field = match definition.name {
                "task show" => "taskId",
                "result show" => "resultId",
                "decision show" => "decisionId",
                "review show" => "candidateId",
                "operation show" => "operationId",
                "workspace get" => "workspaceId",
                _ => "artifactId",
            };
            operation.params[field] = json!(a.id()?);
            Action::Operation(operation)
        }
        "project show" => {
            let project = if a.positional.is_empty() {
                a.take("project")
                    .or_else(|| context.project_id.clone())
                    .with_context(|| t!("agent_center.project_required").into_owned())?
            } else {
                a.id()?
            };
            operation.params = json!({"projectId":project});
            Action::Operation(operation)
        }
        "decision answer" | "intake answer" | "intake cancel" => {
            let id = a.id()?;
            let decision = definition.name == "decision answer";
            let kind = if decision {
                "DecisionRequest"
            } else {
                "IntakeRequest"
            };
            operation
                .if_match
                .push(a.guard(context, kind, &id, "version")?);
            if decision {
                operation.params = json!({"decisionId":id, "value":serde_json::from_str::<Value>(&a.required("value")?)?});
            } else {
                operation.params = json!({"requestId":id,
                    "action":if definition.name == "intake cancel" {"Cancel"} else {"Answer"}});
                if definition.name == "intake answer" {
                    operation.params["value"] = serde_json::from_str(&a.required("value")?)?;
                }
            }
            Action::Operation(operation)
        }
        "review accept" | "review revise" => {
            let candidate = a.id()?;
            let work = a.work(context)?;
            operation
                .if_match
                .push(a.guard(context, "Work", &work, "work-version")?);
            operation.if_match.push(a.guard(
                context,
                "DeliveryCandidate",
                &candidate,
                "version",
            )?);
            operation.params = json!({"candidateId":candidate});
            if definition.name == "review revise" {
                let findings: Value = serde_json::from_str(&a.required("findings")?)?;
                if !findings.as_array().is_some_and(|items| !items.is_empty()) {
                    bail!("{}", t!("agent_center.findings_required"));
                }
                operation.params["findings"] = findings;
                operation.params["preserveArtifacts"] = a
                    .take("preserve")
                    .map(|value| serde_json::from_str(&value))
                    .transpose()?
                    .unwrap_or(json!([]));
                operation.params["advance"] = json!(a.take("advance").is_some());
            }
            Action::Operation(operation)
        }
        "workspace show" => {
            operation.params = json!({"workId":a.work(context)?});
            Action::Operation(operation)
        }
        _ => Action::Unsupported(definition.name.into()),
    };
    a.finish()?;
    if let Some(command_id) = command_id {
        match &mut action {
            Action::Operation(operation) if operation.mutation => operation.command_id = command_id,
            Action::PrepareApply(target) => target.command_id = command_id,
            Action::PrepareTransfer(target) => target.command_id = command_id,
            Action::PrepareProposal(target) => target.command_id = command_id,
            _ => bail!("{}", t!("agent_center.command_id_mutation_only")),
        }
    }
    if !interactive && matches!(&action, Action::Operation(operation) if operation.confirmation) {
        bail!("{}", t!("agent_center.confirmation_required"));
    }
    Ok(action)
}

fn required_id(value: &Value, field: &str) -> Result<String> {
    value[field]
        .as_str()
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .with_context(|| t!("agent_center.missing_field", field = field).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(text: &str) -> Vec<String> {
        tokenize(text).unwrap()
    }
    fn context() -> CommandContext {
        CommandContext {
            work_id: Some("selected".into()),
            project_id: Some("project".into()),
            versions: BTreeMap::from([(("Work".into(), "selected".into()), 3)]),
            ..Default::default()
        }
    }

    #[test]
    fn global_conversation_omits_missing_project_and_preserves_legacy_project_requirement() {
        let _locale = crate::test_support::lock_locale();
        let mut context = CommandContext {
            conversation_id: uuid::Uuid::new_v4().to_string(),
            console_session_id: uuid::Uuid::new_v4().to_string(),
            ..Default::default()
        };
        assert!(!context.global_conversation);
        assert!(conversation("Arrange my reports".into(), &context, false).is_err());
        context.global_conversation = true;
        let request = conversation("Arrange my reports".into(), &context, false).unwrap();
        assert_eq!(request.params["context"]["scope"], "Global");
        assert!(request.params["context"].get("projectId").is_none());
        assert!(request.params["context"].get("selectedWorkId").is_none());
        assert_eq!(
            request.params["context"]["consoleSessionId"],
            context.console_session_id
        );
        context.project_id = Some("project-hint".into());
        context.work_id = Some("work-hint".into());
        let hinted = conversation("Query all works".into(), &context, false).unwrap();
        assert_eq!(hinted.params["context"]["scope"], "Global");
        assert_eq!(hinted.params["context"]["projectId"], "project-hint");
        assert_eq!(hinted.params["context"]["selectedWorkId"], "work-hint");
        assert_eq!(
            hinted.params["conversationId"],
            request.params["conversationId"]
        );
        context.global_conversation = false;
        let legacy = conversation("Legacy scoped request".into(), &context, false).unwrap();
        assert!(legacy.params["context"].get("scope").is_none());
    }

    #[test]
    fn global_precise_intake_triggers_keep_the_lifetime_console_and_conversation_pair() {
        let _locale = crate::test_support::lock_locale();
        let mut context = context();
        context.global_conversation = true;
        context.conversation_id = uuid::Uuid::new_v4().to_string();
        context.console_session_id = uuid::Uuid::new_v4().to_string();
        let original = (
            context.conversation_id.clone(),
            context.console_session_id.clone(),
        );
        for command in [
            "work new \"New report\" --project other-project",
            "plan revise \"Narrow scope\" --work other-work --project other-project",
        ] {
            let op = operation(compile(&args(command), &context, true).unwrap());
            assert_eq!(op.params["conversationId"], original.0);
            assert_eq!(op.params["context"]["consoleSessionId"], original.1);
            assert_eq!(op.params["context"]["scope"], "Global");
        }
        let foreign = uuid::Uuid::new_v4().to_string();
        assert!(compile(
            &args(&format!("work new goal --conversation {foreign}")),
            &context,
            true
        )
        .is_err());
        assert!(compile(
            &args(&format!("plan revise goal --conversation {foreign}")),
            &context,
            true
        )
        .is_err());
        let Action::PrepareProposal(intake) = compile(
            &args("work revise Clarify scope --project other-project"),
            &context,
            true,
        )
        .unwrap() else {
            panic!("expected precise revision preparation");
        };
        assert_eq!(intake.context.conversation_id, original.0);
        assert_eq!(intake.context.console_session_id, original.1);
        assert!(intake.context.global_conversation);
    }

    fn operation(action: Action) -> Operation {
        let Action::Operation(operation) = action else {
            panic!("expected operation")
        };
        operation
    }
    #[test]
    fn ordinary_apply_captures_selection_and_cli_requires_explicit_confirmation() {
        let _locale = crate::test_support::lock_locale();
        let mut captured = context();
        captured
            .versions
            .insert(("ChangeProposal".into(), "change-a".into()), 2);
        let Action::PrepareApply(target) =
            compile(&args("work apply change-a"), &captured, true).unwrap()
        else {
            panic!("expected automatic grant preparation");
        };
        assert_eq!(target.work_id, "selected");
        assert_eq!(target.proposal_id, "change-a");
        assert_eq!(target.work_version, Some(3));
        assert_eq!(target.proposal_version, Some(2));
        assert_eq!(
            complete_with_context("/work apply change", &captured),
            ["/work apply change-a"]
        );
        assert!(compile(
            &args("work apply change-a --work explicit"),
            &CommandContext::default(),
            false
        )
        .is_err());
        assert!(compile(
            &args("work apply change-a --confirm"),
            &CommandContext::default(),
            false
        )
        .is_err());
        let command_id = uuid::Uuid::new_v4().to_string();
        let Action::PrepareApply(target) = compile(
            &args(&format!(
                "work apply change-a --work explicit --confirm --command-id {command_id}"
            )),
            &CommandContext::default(),
            false,
        )
        .unwrap() else {
            panic!("expected CLI preparation")
        };
        assert_eq!(target.work_id, "explicit");
        assert_eq!(target.command_id, command_id);
        assert_eq!(target.work_version, None);
    }

    #[test]
    fn ordinary_work_revision_captures_text_and_existing_conversation() {
        let _locale = crate::test_support::lock_locale();
        let mut captured = context();
        captured.console_session_id = uuid::Uuid::new_v4().to_string();
        captured.conversation_id = uuid::Uuid::new_v4().to_string();
        let message = uuid::Uuid::new_v4().to_string();
        let Action::PrepareProposal(intake) = compile(
            &args(&format!(
            "work revise narrow scope without changing acceptance criteria --message-id {message}"
        )),
            &captured,
            true,
        )
        .unwrap() else {
            panic!("expected proposal-only intake preparation")
        };
        assert_eq!(intake.context.work_id, captured.work_id);
        assert_eq!(intake.context.project_id, captured.project_id);
        assert_eq!(
            intake.context.console_session_id,
            captured.console_session_id
        );
        assert_eq!(intake.context.conversation_id, captured.conversation_id);
        assert_eq!(intake.client_message_id, message);
        assert_eq!(
            intake.text,
            "narrow scope without changing acceptance criteria"
        );
        captured.work_id = Some("other".into());
        assert_eq!(intake.context.work_id.as_deref(), Some("selected"));
    }

    #[test]
    fn changed_intake_scopes_never_reuse_another_conversations_console_binding() {
        let _locale = crate::test_support::lock_locale();
        let captured = context();
        for command in [
            "work new another-goal",
            "work new another-goal --conversation explicit-conversation",
            "plan revise new-plan --work another-work",
            "plan revise new-plan --conversation explicit-conversation",
            "work revise new-scope --work another-work",
            "work revise new-scope --conversation explicit-conversation",
        ] {
            let action = compile(&args(command), &captured, true).unwrap();
            let (conversation, console) = match action {
                Action::Operation(operation) => (
                    operation.params["conversationId"]
                        .as_str()
                        .unwrap()
                        .to_owned(),
                    operation.params["context"]["consoleSessionId"]
                        .as_str()
                        .unwrap()
                        .to_owned(),
                ),
                Action::PrepareProposal(intake) => (
                    intake.context.conversation_id,
                    intake.context.console_session_id,
                ),
                _ => panic!("expected intake: {command}"),
            };
            assert_ne!(conversation, captured.conversation_id, "{command}");
            assert_ne!(console, captured.console_session_id, "{command}");
            assert_ne!(console, conversation, "{command}");
            uuid::Uuid::parse_str(&console).unwrap();
        }
        let mut home = captured;
        home.work_id = None;
        let Action::Operation(operation) = compile(
            &args("work new another-goal --project another-project"),
            &home,
            true,
        )
        .unwrap() else {
            panic!("expected project-scoped intake");
        };
        assert_ne!(
            operation.params["conversationId"],
            json!(home.conversation_id)
        );
        assert_ne!(
            operation.params["context"]["consoleSessionId"],
            json!(home.console_session_id)
        );
    }

    #[test]
    fn cli_work_revision_requires_work_and_keeps_distinct_stable_intake_identities() {
        let _locale = crate::test_support::lock_locale();
        assert!(compile(&args("work revise narrow scope"), &context(), false).is_err());
        let command_id = uuid::Uuid::new_v4().to_string();
        let arguments = args(&format!(
            "work revise 'narrow scope' --work explicit --command-id {command_id}"
        ));
        let Action::PrepareProposal(first) =
            compile(&arguments, &CommandContext::default(), false).unwrap()
        else {
            panic!("expected CLI proposal preparation");
        };
        let Action::PrepareProposal(retry) =
            compile(&arguments, &CommandContext::default(), false).unwrap()
        else {
            panic!("expected repeatable CLI proposal preparation");
        };
        assert_eq!(first.context.work_id.as_deref(), Some("explicit"));
        assert_eq!(first.context.project_id, None);
        assert_eq!(first.command_id, command_id);
        assert_eq!(
            first.context.console_session_id,
            retry.context.console_session_id
        );
        assert_eq!(first.context.conversation_id, retry.context.conversation_id);
        assert_eq!(first.client_message_id, retry.client_message_id);
        assert_ne!(
            first.context.console_session_id,
            first.context.conversation_id
        );
        for id in [
            &first.context.console_session_id,
            &first.context.conversation_id,
            &first.client_message_id,
        ] {
            uuid::Uuid::parse_str(id).unwrap();
        }
    }

    #[test]
    fn apply_request_document_bypasses_preparation_and_retains_all_authority() {
        let _locale = crate::test_support::lock_locale();
        let document = json!({"method":"work.apply_change","commandId":uuid::Uuid::new_v4().to_string(),
            "params":{"proposalId":"change-a","grantProposalId":"caller-grant"},
            "ifMatch":[{"kind":"Work","id":"caller-work","version":17},
                {"kind":"ChangeProposal","id":"change-a","version":11}]});
        let args = vec![
            "work".into(),
            "apply".into(),
            "change-a".into(),
            "--request-json".into(),
            document.to_string(),
        ];
        let op = operation(compile(&args, &context(), true).unwrap());
        assert_eq!(op.params, document["params"]);
        assert_eq!(
            op.if_match,
            document["ifMatch"].as_array().unwrap().to_vec()
        );
        assert_eq!(op.command_id, document["commandId"].as_str().unwrap());
        assert!(op.confirmation);
    }
    #[test]
    fn parser_handles_quotes_literals_and_windows_paths() {
        let _locale = crate::test_support::lock_locale();
        assert_eq!(args("work new \"a goal\""), ["work", "new", "a goal"]);
        assert_eq!(
            args(r#"artifact show 'C:\repo\file'"#),
            ["artifact", "show", r"C:\repo\file"]
        );
        assert_eq!(
            args(r"artifact show \\server\share"),
            ["artifact", "show", r"\\server\share"]
        );
        assert!(tokenize("work new 'unfinished").is_err());
        assert_eq!(
            parse_input("  //work new").unwrap(),
            Input::Text("/work new".into())
        );
        assert!(matches!(
            parse_input("```\n/work start\n```").unwrap(),
            Input::Text(_)
        ));
    }
    #[test]
    fn explicit_target_wins_and_missing_target_is_not_guessed() {
        let _locale = crate::test_support::lock_locale();
        let op = operation(compile(&args("work show --work explicit"), &context(), false).unwrap());
        assert_eq!(op.params["workId"], "explicit");
        assert!(compile(&args("task list"), &CommandContext::default(), false).is_err());
    }
    #[test]
    fn mutation_captures_target_and_version() {
        let _locale = crate::test_support::lock_locale();
        let mut context = context();
        let op = operation(compile(&args("work pause"), &context, false).unwrap());
        context.work_id = Some("other".into());
        assert_eq!(op.params["workId"], "selected");
        assert_eq!(op.if_match[0]["version"], 3);
        assert_eq!(op.params["action"], "Hold");
    }
    #[test]
    fn new_work_is_intake_without_invented_contract() {
        let _locale = crate::test_support::lock_locale();
        let op = operation(compile(&args("work new 'fix exports'"), &context(), false).unwrap());
        assert_eq!(op.method, "conversation.submit");
        assert_eq!(op.params["declaredIntent"], "NewWork");
        assert!(op.params["context"].get("selectedWorkId").is_none());
        assert!(op.params.get("criteria").is_none());
    }
    #[test]
    fn final_review_never_maps_to_internal_review() {
        let _locale = crate::test_support::lock_locale();
        let op = operation(
            compile(
                &args("review accept candidate --version 4 --work-version 3 --confirm"),
                &context(),
                false,
            )
            .unwrap(),
        );
        assert_eq!(op.method, "delivery.accept");
        assert_eq!(op.if_match.len(), 2);
        assert!(compile(
            &args("review accept candidate --version 4 --work-version 3"),
            &context(),
            false
        )
        .is_err());
    }
    #[test]
    fn invalid_arguments_and_unknown_families_are_explicit() {
        let _locale = crate::test_support::lock_locale();
        assert!(compile(&args("work show --work a --work b"), &context(), false).is_err());
        assert!(compile(&args("work list --typo yes"), &context(), false).is_err());
        assert!(matches!(
            compile(&args("shell open"), &context(), false).unwrap(),
            Action::Unsupported(_)
        ));
        assert!(completions("/review a").contains(&"/review accept".into()));
    }

    #[test]
    fn retry_changes_request_identity_not_command_identity_or_payload() {
        let _locale = crate::test_support::lock_locale();
        let command = "f4d9e890-5398-4c5a-b620-d0d8355c3e51";
        let input = args(&format!(
            "work new 'fix exports' --project project --command-id {command}"
        ));
        let first = operation(compile(&input, &CommandContext::default(), false).unwrap());
        let second = operation(compile(&input, &CommandContext::default(), false).unwrap());
        let first_frame = first.envelope();
        let second_frame = second.envelope();
        assert_eq!(first_frame["commandId"], second_frame["commandId"]);
        assert_ne!(first_frame["requestId"], second_frame["requestId"]);
        assert_eq!(first.params, second.params);
        assert_eq!(first.if_match, second.if_match);
    }

    #[test]
    fn final_revision_is_criterion_bound_and_does_not_resume_by_default() {
        let _locale = crate::test_support::lock_locale();
        let input = args(
            r#"review revise candidate --findings '[{"criterionId":"rows","requestedChange":"include final row"}]' --version 4 --work-version 3 --confirm"#,
        );
        let op = operation(compile(&input, &context(), false).unwrap());
        assert_eq!(op.method, "delivery.request_changes");
        assert_eq!(op.params["advance"], false);
        assert_eq!(op.params["preserveArtifacts"], json!([]));
        assert_eq!(op.params["findings"][0]["criterionId"], "rows");
    }

    #[test]
    fn completion_targets_come_from_captured_records_not_shell_focus() {
        let _locale = crate::test_support::lock_locale();
        assert_eq!(
            complete_with_context("/work use sel", &context()),
            ["/work use selected"]
        );
        assert_eq!(
            complete_with_context("/work pause --work ", &context()),
            ["/work pause --work selected"]
        );
        assert!(complete_with_context("/decision answer ", &context()).is_empty());
    }

    #[test]
    fn structured_families_expose_the_same_registry_help() {
        let _locale = crate::test_support::lock_locale();
        assert!(
            matches!(compile(&args("work help"), &context(), false).unwrap(), Action::Help(prefix) if prefix == "work")
        );
        assert!(
            matches!(compile(&args("review accept --help"), &context(), false).unwrap(), Action::Help(prefix) if prefix == "review accept")
        );
        assert_eq!(
            operation(compile(&args("work new help"), &context(), false).unwrap()).params["text"],
            "help"
        );
        assert_eq!(
            unsupported_failure("shell open").0,
            "CAPABILITY_UNAVAILABLE"
        );
        assert_eq!(
            unsupported_failure("run retry attempt").0,
            "METHOD_UNSUPPORTED"
        );
    }

    #[test]
    fn plan_revision_is_a_captured_coordinator_request_not_an_applied_plan() {
        let _locale = crate::test_support::lock_locale();
        let op = operation(
            compile(
                &args("plan revise 'rerun only the failed check' --work other"),
                &context(),
                false,
            )
            .unwrap(),
        );
        assert_eq!(op.method, "conversation.submit");
        assert_eq!(op.params["declaredIntent"], "WorkDiscussion");
        assert_eq!(op.params["context"]["selectedWorkId"], "other");
        assert_eq!(op.params["text"], "rerun only the failed check");
        assert!(op.params.get("tasks").is_none());
        assert!(op.if_match.is_empty());
    }

    #[test]
    fn project_bootstrap_requires_human_confirmation_and_preserves_typed_limits() {
        let _locale = crate::test_support::lock_locale();
        let payload = json!({"name":"project","root":"C:\\repo","coordinatorCapabilityId":"coordinator",
            "workerCapabilityId":"worker","checkCapabilityId":"check",
            "limits":{"concurrency":2,"executionAttempts":3,"evaluationAttempts":4,"coordinationTurns":5,
                "contextRounds":2,"executionSeconds":60,"coordinationSeconds":30}});
        let mut input = vec![
            "project".into(),
            "configure".into(),
            "--params-json".into(),
            payload.to_string(),
        ];
        let op = operation(compile(&input, &CommandContext::default(), true).unwrap());
        assert_eq!(op.method, "project.configure");
        assert_eq!(op.params, payload);
        assert!(op.if_match.is_empty());
        assert!(op.confirmation);
        let quoted = args(&format!("project configure --params-json '{payload}'"));
        assert_eq!(
            operation(compile(&quoted, &CommandContext::default(), true).unwrap()).params,
            payload
        );
        assert!(compile(&input, &CommandContext::default(), false).is_err());
        input.push("--confirm".into());
        assert!(
            !operation(compile(&input, &CommandContext::default(), false).unwrap()).confirmation
        );
    }

    #[test]
    fn manual_transfer_guards_exact_workspace_and_never_infers_resume() {
        let _locale = crate::test_support::lock_locale();
        for (command, payload) in [
            ("takeover", json!({"workspaceId":"workspace"})),
            (
                "handback",
                json!({"workspaceId":"workspace","summary":"updated report","resumeAffected":false}),
            ),
        ] {
            let input = vec![
                "workspace".into(),
                command.into(),
                "--params-json".into(),
                payload.to_string(),
                "--version".into(),
                "8".into(),
                "--confirm".into(),
            ];
            let op = operation(compile(&input, &context(), false).unwrap());
            assert_eq!(op.method, format!("workspace.{command}"));
            assert_eq!(op.params, payload);
            assert_eq!(
                op.if_match,
                [json!({"kind":"Workspace","id":"workspace","version":8})]
            );
        }
    }

    #[test]
    fn spec_proposal_and_approval_are_distinct_guarded_operations() {
        let _locale = crate::test_support::lock_locale();
        let payload = json!({"replacementSpec":{"revision":1,"goal":"user supplied changed goal"},
            "affectedTaskIds":["task"],"reason":"human requested scope"});
        let op = operation(
            compile(
                &[
                    "work".into(),
                    "revise".into(),
                    "--params-json".into(),
                    payload.to_string(),
                ],
                &context(),
                false,
            )
            .unwrap(),
        );
        assert_eq!(op.method, "work.propose_change");
        assert_eq!(op.params["replacementSpec"], payload["replacementSpec"]);
        assert_eq!(op.params["workId"], "selected");
        assert_eq!(
            op.if_match,
            [json!({"kind":"Work","id":"selected","version":3})]
        );
        let approval = json!({"proposalId":"proposal","grantProposalId":"grant"});
        let op = operation(
            compile(
                &[
                    "work".into(),
                    "apply".into(),
                    "--params-json".into(),
                    approval.to_string(),
                    "--work-version".into(),
                    "3".into(),
                    "--proposal-version".into(),
                    "2".into(),
                    "--confirm".into(),
                ],
                &context(),
                false,
            )
            .unwrap(),
        );
        assert_eq!(op.method, "work.apply_change");
        assert_eq!(op.params, approval);
        assert_eq!(
            op.if_match,
            [
                json!({"kind":"Work","id":"selected","version":3}),
                json!({"kind":"ChangeProposal","id":"proposal","version":2})
            ]
        );
    }
    #[test]
    fn inbox_is_global_unless_explicitly_scoped() {
        let _locale = crate::test_support::lock_locale();
        let context = CommandContext {
            work_id: Some("selected".into()),
            ..Default::default()
        };
        for (args, expected) in [
            (vec!["inbox".into()], json!({"limit":100})),
            (
                vec!["inbox".into(), "--work".into(), "other".into()],
                json!({"limit":100,"workId":"other"}),
            ),
        ] {
            let Action::Operation(operation) = compile(&args, &context, true).unwrap() else {
                panic!("expected read")
            };
            assert_eq!(operation.params, expected);
        }
    }

    #[test]
    fn selected_work_transfer_is_guided_and_cli_requires_confirmation() {
        let _locale = crate::test_support::lock_locale();
        let context = CommandContext {
            work_id: Some("selected".into()),
            ..Default::default()
        };
        let args = vec!["workspace".into(), "takeover".into()];
        assert!(compile(&args, &context, false).is_err());
        let Action::PrepareTransfer(target) = compile(&args, &context, true).unwrap() else {
            panic!("expected preparation")
        };
        assert_eq!(target.work_id, "selected");
        assert!(target.handback.is_none());
        let args = vec![
            "workspace".into(),
            "handback".into(),
            "--work".into(),
            "explicit".into(),
            "--summary".into(),
            "  preserved summary\n".into(),
            "--resume-affected".into(),
            "true".into(),
            "--confirm".into(),
        ];
        let Action::PrepareTransfer(target) = compile(&args, &context, false).unwrap() else {
            panic!("expected preparation")
        };
        assert_eq!(target.work_id, "explicit");
        assert_eq!(
            target.handback,
            Some(("  preserved summary\n".into(), true))
        );
        assert!(compile(&["workspace".into(), "handback".into()], &context, true).is_err());
        assert!(compile(
            &[
                "workspace".into(),
                "handback".into(),
                "--summary".into(),
                "updated input".into()
            ],
            &context,
            true,
        )
        .is_err());
        let Action::PrepareTransfer(target) = compile(
            &[
                "workspace".into(),
                "handback".into(),
                "--summary".into(),
                "updated input".into(),
                "--resume-affected".into(),
                "false".into(),
            ],
            &context,
            true,
        )
        .unwrap() else {
            panic!("expected explicit manual hold")
        };
        assert_eq!(target.handback, Some(("updated input".into(), false)));
    }
}
