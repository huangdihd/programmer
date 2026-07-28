// Copyright (C) 2026 huangdihd
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

use crate::providers::ProviderManager;
use async_openai::types::responses::{ImageDetail, InputImageContent};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use tokio::io::AsyncReadExt;

// ---------------------------------------------------------------------------
// Command parsing
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Quit,
    Clear,
    New,
    Model(String),
    /// `/providers <subcommand>` — carries the raw argument string
    /// ("show", "manage", or anything else for the usage hint).
    Providers(String),
    /// `/mode <manual|auto|plan|yolo>` — cycle/set work mode.
    Mode(String),
    /// `/classifier [provider/model]` — set or show the Auto-mode classifier
    /// model. Empty argument shows the current setting; "clear"/"default"
    /// resets it to the chat model.
    Classifier(String),
    /// `/init` — have the agent explore the project, write `PROGRAMMER.md`, and
    /// configure the diagnostics profile.
    Init,
    Help,
    Session,
    Todo,
    /// `/skill <name|list|off>` — activate, list, or clear skills.
    Skill(String),
    /// `/mcp <show|manage>` — list or manage MCP servers.
    Mcp(String),
    /// `/plan <approve|cancel>` — plan mode control.
    Plan(String),
    /// `/terminal [id]` — open the interactive terminal panel for a task.
    Terminal(String),
    /// `/compact [provider/model]` — summarize the conversation so far and
    /// shrink the context the model sees to that summary plus everything
    /// after it. The optional argument picks a different model for the
    /// summarization request only.
    Compact(String),
    /// `/thinking [level]` — set or show the reasoning effort used by the main
    /// conversation and `/compact`.
    Thinking(String),
    /// `/vision <on|off>` — enable or disable image attachments for this session.
    Vision(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CommandKind {
    Quit,
    Clear,
    New,
    Model,
    Providers,
    Mode,
    Classifier,
    Init,
    Help,
    Session,
    Todo,
    Skill,
    Mcp,
    Plan,
    Terminal,
    Compact,
    Thinking,
    Vision,
}

#[derive(Debug, Clone, Copy)]
enum CompletionKind {
    None,
    Model,
    Fixed(&'static [&'static str]),
    Skill,
    Terminal,
}

#[derive(Debug, Clone, Copy)]
struct HelpEntry {
    order: u8,
    usage: &'static str,
    description: &'static str,
}

#[derive(Debug, Clone, Copy)]
struct CommandSpec {
    kind: CommandKind,
    name: &'static str,
    aliases: &'static [&'static str],
    completion: CompletionKind,
    help: &'static [HelpEntry],
}

impl CommandKind {
    fn command(self, args: String) -> Command {
        match self {
            Self::Quit => Command::Quit,
            Self::Clear => Command::Clear,
            Self::New => Command::New,
            Self::Model => Command::Model(args),
            Self::Providers => Command::Providers(args),
            Self::Mode => Command::Mode(args),
            Self::Classifier => Command::Classifier(args),
            Self::Init => Command::Init,
            Self::Help => Command::Help,
            Self::Session => Command::Session,
            Self::Todo => Command::Todo,
            Self::Skill => Command::Skill(args),
            Self::Mcp => Command::Mcp(args),
            Self::Plan => Command::Plan(args),
            Self::Terminal => Command::Terminal(args),
            Self::Compact => Command::Compact(args),
            Self::Thinking => Command::Thinking(args),
            Self::Vision => Command::Vision(args),
        }
    }
}

impl CommandSpec {
    fn matches(&self, name: &str) -> bool {
        self.name == name || self.aliases.contains(&name)
    }
}

const COMMAND_SPECS: &[CommandSpec] = &[
    CommandSpec {
        kind: CommandKind::Model,
        name: "model",
        aliases: &["m"],
        completion: CompletionKind::Model,
        help: &[HelpEntry {
            order: 0,
            usage: "/model <provider/model>",
            description: "Switch to a different model",
        }],
    },
    CommandSpec {
        kind: CommandKind::New,
        name: "new",
        aliases: &["n"],
        completion: CompletionKind::None,
        help: &[HelpEntry {
            order: 15,
            usage: "/new | /n",
            description: "Start a new session (saves current)",
        }],
    },
    CommandSpec {
        kind: CommandKind::Providers,
        name: "providers",
        aliases: &["provider"],
        completion: CompletionKind::Fixed(&["show", "manage"]),
        help: &[
            HelpEntry {
                order: 16,
                usage: "/providers show",
                description: "List all configured providers and models",
            },
            HelpEntry {
                order: 17,
                usage: "/providers manage",
                description: "Open the provider management panel",
            },
        ],
    },
    CommandSpec {
        kind: CommandKind::Session,
        name: "session",
        aliases: &["s"],
        completion: CompletionKind::None,
        help: &[HelpEntry {
            order: 18,
            usage: "/session | /s",
            description: "Show current session info",
        }],
    },
    CommandSpec {
        kind: CommandKind::Mode,
        name: "mode",
        aliases: &[],
        completion: CompletionKind::Fixed(&["manual", "auto", "plan", "yolo"]),
        help: &[HelpEntry {
            order: 1,
            usage: "/mode <manual|auto|plan|yolo>",
            description: "Set work mode (or cycle with Ctrl+T)",
        }],
    },
    CommandSpec {
        kind: CommandKind::Classifier,
        name: "classifier",
        aliases: &[],
        completion: CompletionKind::Model,
        help: &[HelpEntry {
            order: 2,
            usage: "/classifier [provider/model]",
            description: "Set/show the Auto-mode classifier model",
        }],
    },
    CommandSpec {
        kind: CommandKind::Init,
        name: "init",
        aliases: &[],
        completion: CompletionKind::None,
        help: &[HelpEntry {
            order: 3,
            usage: "/init",
            description: "Explore the project, write PROGRAMMER.md, set up diagnostics",
        }],
    },
    CommandSpec {
        kind: CommandKind::Todo,
        name: "todo",
        aliases: &["todos", "t"],
        completion: CompletionKind::None,
        help: &[HelpEntry {
            order: 14,
            usage: "/todo | /t",
            description: "Open the todo list panel",
        }],
    },
    CommandSpec {
        kind: CommandKind::Skill,
        name: "skill",
        aliases: &["skills"],
        completion: CompletionKind::Skill,
        help: &[
            HelpEntry {
                order: 6,
                usage: "/skill <name|list|off>",
                description: "Activate, list, or clear agent skills",
            },
            HelpEntry {
                order: 7,
                usage: "/skill manage",
                description: "Open the skills management panel",
            },
        ],
    },
    CommandSpec {
        kind: CommandKind::Mcp,
        name: "mcp",
        aliases: &[],
        completion: CompletionKind::Fixed(&["show", "manage"]),
        help: &[
            HelpEntry {
                order: 8,
                usage: "/mcp show",
                description: "List configured MCP servers and their status",
            },
            HelpEntry {
                order: 9,
                usage: "/mcp manage",
                description: "Open the MCP server management panel",
            },
        ],
    },
    CommandSpec {
        kind: CommandKind::Plan,
        name: "plan",
        aliases: &[],
        completion: CompletionKind::Fixed(&["approve", "cancel"]),
        help: &[
            HelpEntry {
                order: 4,
                usage: "/plan approve",
                description: "Approve the current plan (Plan mode)",
            },
            HelpEntry {
                order: 5,
                usage: "/plan cancel",
                description: "Cancel plan and return to Auto mode",
            },
        ],
    },
    CommandSpec {
        kind: CommandKind::Terminal,
        name: "terminal",
        aliases: &["term"],
        completion: CompletionKind::Terminal,
        help: &[HelpEntry {
            order: 10,
            usage: "/terminal [id]",
            description: "Open the interactive terminal for a PTY task",
        }],
    },
    CommandSpec {
        kind: CommandKind::Compact,
        name: "compact",
        aliases: &[],
        completion: CompletionKind::Model,
        help: &[HelpEntry {
            order: 11,
            usage: "/compact [provider/model]",
            description: "Summarize older history to shrink the model's context",
        }],
    },
    CommandSpec {
        kind: CommandKind::Thinking,
        name: "thinking",
        aliases: &[],
        completion: CompletionKind::Fixed(crate::thinking::ThinkingLevel::COMPLETIONS),
        help: &[HelpEntry {
            order: 12,
            usage: "/thinking [auto|none|minimal|low|medium|high|xhigh]",
            description: "Set/show reasoning effort for chat and compaction",
        }],
    },
    CommandSpec {
        kind: CommandKind::Vision,
        name: "vision",
        aliases: &[],
        completion: CompletionKind::Fixed(&["on", "off"]),
        help: &[HelpEntry {
            order: 13,
            usage: "/vision <on|off>",
            description: "Enable or disable image attachments for this session",
        }],
    },
    CommandSpec {
        kind: CommandKind::Clear,
        name: "clear",
        aliases: &["c"],
        completion: CompletionKind::None,
        help: &[HelpEntry {
            order: 19,
            usage: "/clear | /c",
            description: "Clear the conversation history",
        }],
    },
    CommandSpec {
        kind: CommandKind::Quit,
        name: "quit",
        aliases: &["q", "exit"],
        completion: CompletionKind::None,
        help: &[HelpEntry {
            order: 20,
            usage: "/quit | /q",
            description: "Exit the application",
        }],
    },
    CommandSpec {
        kind: CommandKind::Help,
        name: "help",
        aliases: &["?"],
        completion: CompletionKind::None,
        help: &[HelpEntry {
            order: 21,
            usage: "/help | /?",
            description: "Show this help",
        }],
    },
];

impl Command {
    /// Parse a slash-command from user input. Returns `None` if the input does
    /// not start with `/` or if the command name is not recognised (in which
    /// case the caller may choose to forward it to the AI as a normal message).
    pub fn parse(input: &str) -> Option<Self> {
        let input = input.trim();
        if !input.starts_with('/') {
            return None;
        }

        let (cmd, args) = if let Some((cmd, rest)) = input[1..].split_once(char::is_whitespace) {
            (cmd, rest.trim().to_string())
        } else {
            (&input[1..], String::new())
        };

        COMMAND_SPECS
            .iter()
            .find(|spec| spec.matches(cmd))
            .map(|spec| spec.kind.command(args))
    }

    /// All command names (without leading `/`), for completion.
    pub fn all_commands() -> impl Iterator<Item = &'static str> {
        COMMAND_SPECS.iter().map(|spec| spec.name)
    }

    /// Human-readable descriptions for the help text.
    pub fn descriptions() -> Vec<(&'static str, &'static str)> {
        let mut entries: Vec<_> = COMMAND_SPECS.iter().flat_map(|spec| spec.help).collect();
        entries.sort_by_key(|entry| entry.order);
        entries
            .into_iter()
            .map(|entry| (entry.usage, entry.description))
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Completion engine
// ---------------------------------------------------------------------------

/// Snapshot of the current completion candidates and selection.
#[derive(Debug, Clone)]
pub struct CompletionState {
    /// Input text before the token being completed; accepting candidate `i`
    /// produces `prefix + candidates[i]`.
    pub prefix: String,
    /// Candidates for the current token only (this is what the popup shows).
    pub candidates: Vec<String>,
    /// Index of the currently-highlighted candidate.
    pub selected: usize,
    /// Whether the popup is visible (first Tab shows it).
    pub visible: bool,
    /// Scroll offset for the popup (how many items are scrolled off the top).
    pub scroll_offset: usize,
}

impl CompletionState {
    fn new(prefix: String, candidates: Vec<String>) -> Option<Self> {
        if candidates.is_empty() {
            return None;
        }
        Some(CompletionState {
            prefix,
            candidates,
            selected: 0,
            visible: true,
            scroll_offset: 0,
        })
    }

    /// The full input line that accepting candidate `i` produces.
    pub fn line(&self, i: usize) -> String {
        format!("{}{}", self.prefix, self.candidates[i])
    }
}

/// Stateless engine that computes tab-completion candidates from the current
/// input and the provider registry.
pub struct CompletionEngine;

impl CompletionEngine {
    /// Compute completion candidates from the current input line.
    ///
    /// Returns `None` when the input does not trigger completions (e.g. doesn't
    /// start with `/`) or when no candidates match.
    pub(crate) fn complete(
        input: &str,
        pm: &ProviderManager,
        skill_registry: &crate::skills::SkillRegistry,
    ) -> Option<CompletionState> {
        if !input.starts_with('/') {
            return None;
        }

        let text = &input[1..]; // strip leading '/'
        let parts: Vec<&str> = text.split_whitespace().collect();

        if parts.is_empty() || (parts.len() == 1 && !text.ends_with(char::is_whitespace)) {
            // Completing the command name itself.
            let typed = parts.first().copied().unwrap_or("");
            let candidates: Vec<String> = Command::all_commands()
                .filter(|c| c.starts_with(typed))
                .map(|c| format!("/{}", c))
                .collect();
            return CompletionState::new(String::new(), candidates);
        }

        let cmd = parts[0];
        let spec = COMMAND_SPECS.iter().find(|spec| spec.matches(cmd))?;
        match spec.completion {
            CompletionKind::None => None,
            CompletionKind::Model => Self::complete_model(text, cmd, pm),
            CompletionKind::Fixed(values) => Self::complete_subcommand(text, cmd, values),
            CompletionKind::Skill => Self::complete_skill(text, cmd, skill_registry),
            CompletionKind::Terminal => Self::complete_terminal(text, cmd),
        }
    }

    /// Complete a `/terminal` task id from all running tasks. Each candidate is
    /// `"<id>  <name>"`; the id is the first token so it still parses when
    /// accepted with the name appended.
    fn complete_terminal(text: &str, cmd: &str) -> Option<CompletionState> {
        let after_cmd = text[cmd.len()..].trim_start();
        let prefix = format!("/{} ", cmd);
        let candidates: Vec<String> = crate::tasks::snapshot_all()
            .iter()
            .filter(|t| t.status == crate::tasks::TaskStatus::Running)
            .map(|t| format!("{}  {}", t.id, t.name))
            .filter(|c| c.starts_with(after_cmd))
            .collect();
        CompletionState::new(prefix, candidates)
    }

    /// Complete an `@file` reference. Triggered when the whitespace-delimited
    /// token at the end of the input begins with `@`; the part after `@` is
    /// treated as a (possibly partial) path relative to the working directory.
    pub(crate) fn complete_file_ref(content: &str) -> Option<CompletionState> {
        let (prefix, partial) = active_at_token(content)?;
        let candidates = list_path_candidates(&partial);
        CompletionState::new(prefix, candidates)
    }

    /// Complete a `!command` line, shell-style: the first word completes
    /// against the executables on `PATH` (or as a path when it contains `/`),
    /// later words complete as file paths.
    pub(crate) fn complete_bang(content: &str) -> Option<CompletionState> {
        let after = content.strip_prefix('!')?;
        let token_start = after.rfind(char::is_whitespace).map(|i| i + 1).unwrap_or(0);
        let token = &after[token_start..];
        let prefix = format!("!{}", &after[..token_start]);
        let completing_command = token_start == 0;

        let candidates = if completing_command && !token.contains('/') {
            if token.is_empty() {
                // A bare `!` would list every command on PATH — pure noise.
                return None;
            }
            path_executables()
                .iter()
                .filter(|c| c.starts_with(token))
                .take(50)
                .cloned()
                .collect()
        } else {
            list_path_candidates(token)
        };
        CompletionState::new(prefix, candidates)
    }

    /// Complete a fixed set of subcommands for `cmd`.
    fn complete_subcommand(text: &str, cmd: &str, subcommands: &[&str]) -> Option<CompletionState> {
        let after_cmd = text[cmd.len()..].trim_start();
        let prefix = format!("/{} ", cmd);
        let candidates: Vec<String> = subcommands
            .iter()
            .filter(|s| s.starts_with(after_cmd))
            .map(|s| s.to_string())
            .collect();
        CompletionState::new(prefix, candidates)
    }

    fn complete_model(text: &str, cmd: &str, pm: &ProviderManager) -> Option<CompletionState> {
        let after_cmd = text[cmd.len()..].trim_start();
        // Everything before the argument token stays in the input untouched.
        let prefix = format!("/{} ", cmd);

        // Nothing typed yet after /model — show all models from all providers.
        if after_cmd.is_empty() {
            let mut models: Vec<String> = Vec::new();
            for prov in pm.provider_names() {
                for model in pm.models_for(prov) {
                    models.push(format!("{}/{}", prov, model));
                }
            }
            return CompletionState::new(prefix, models);
        }

        // User typed something after /model. Could be "openai" or "openai/gpt-4o".
        if let Some((prov, partial_model)) = after_cmd.split_once('/') {
            // Already past the / — complete model names.
            let candidates: Vec<String> = pm
                .models_for(prov)
                .iter()
                .filter(|m| m.starts_with(partial_model))
                .map(|m| format!("{}/{}", prov, m))
                .collect();
            return CompletionState::new(prefix, candidates);
        }

        // User is typing a provider name (no '/' yet).
        let providers: Vec<String> = pm
            .provider_names()
            .iter()
            .filter(|p| p.starts_with(after_cmd))
            .map(|p| format!("{}/", p))
            .collect();
        CompletionState::new(prefix, providers)
    }

    fn complete_skill(
        text: &str,
        cmd: &str,
        reg: &crate::skills::SkillRegistry,
    ) -> Option<CompletionState> {
        let after_cmd = text[cmd.len()..].trim_start();
        let prefix = format!("/{} ", cmd);
        let builtins = ["list", "off", "manage"];
        if after_cmd.is_empty() {
            let mut candidates: Vec<String> = builtins.iter().map(|s| s.to_string()).collect();
            for name in reg.names() {
                candidates.push(name.clone());
            }
            return CompletionState::new(prefix, candidates);
        }
        let mut candidates: Vec<String> = builtins
            .iter()
            .filter(|s| s.starts_with(after_cmd))
            .map(|s| s.to_string())
            .collect();
        for name in reg.names() {
            if name.starts_with(after_cmd) {
                candidates.push(name.clone());
            }
        }
        CompletionState::new(prefix, candidates)
    }
}

// ---------------------------------------------------------------------------
// `@file` reference completion + expansion
// ---------------------------------------------------------------------------

/// If the whitespace-delimited token at the end of `content` is an `@file`
/// reference, return `(prefix_including_@, partial_path_after_@)`. The prefix
/// is everything up to and including the `@`, so `prefix + candidate`
/// reconstructs the whole input line.
fn active_at_token(content: &str) -> Option<(String, String)> {
    let token_start = content
        .rfind(char::is_whitespace)
        .map(|i| i + 1)
        .unwrap_or(0);
    let token = &content[token_start..];
    let partial = token.strip_prefix('@')?;
    let prefix = format!("{}@", &content[..token_start]);
    Some((prefix, partial.to_string()))
}

/// Directories skipped when the user hasn't started typing a name — they are
/// large and rarely the intended reference.
const NOISE_DIRS: &[&str] = &["target", "node_modules", ".git"];

/// The user's home directory, from the environment (`HOME`, or `USERPROFILE`
/// on Windows).
fn home_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(Into::into)
}

/// Expand a leading `~/` to the home directory. Any other path (including
/// `~user/` forms) is returned unchanged.
fn expand_tilde(path: &str) -> String {
    match (path.strip_prefix("~/"), home_dir()) {
        (Some(rest), Some(home)) => format!("{}/{rest}", home.to_string_lossy()),
        _ => path.to_string(),
    }
}

/// List path candidates for a (possibly partial) path, shell-completion style:
/// only the directory named by the partial is read (one level), entries are
/// filtered by the trailing name prefix, directories sort first and gain a
/// trailing `/` so completion can descend into them. A leading `~/` is
/// expanded for the directory read but kept verbatim in the candidates.
fn list_path_candidates(partial: &str) -> Vec<String> {
    // A bare `~` can only become the home directory.
    if partial == "~" {
        return vec!["~/".to_string()];
    }
    let (dir_part, name_prefix) = match partial.rfind('/') {
        Some(i) => (&partial[..=i], &partial[i + 1..]),
        None => ("", partial),
    };
    let read_path = if dir_part.is_empty() {
        ".".to_string()
    } else {
        expand_tilde(dir_part)
    };
    let Ok(entries) = std::fs::read_dir(&read_path) else {
        return Vec::new();
    };

    let mut out: Vec<(bool, String)> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        // Hidden entries only when the user explicitly typed a leading dot.
        if name.starts_with('.') && !name_prefix.starts_with('.') {
            continue;
        }
        if !name.starts_with(name_prefix) {
            continue;
        }
        if NOISE_DIRS.contains(&name.as_str()) && name_prefix.is_empty() {
            continue;
        }
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        let mut candidate = format!("{dir_part}{name}");
        if is_dir {
            candidate.push('/');
        }
        out.push((is_dir, candidate));
    }
    // Directories first, then alphabetical; cap the list so the popup stays small.
    out.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    out.into_iter().map(|(_, c)| c).take(50).collect()
}

/// Executable names found on `PATH`, sorted and deduplicated. Scanned once per
/// process (lazily, on the first `!` completion) — PATH changes mid-session are
/// rare enough not to matter.
fn path_executables() -> &'static [String] {
    static CMDS: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    CMDS.get_or_init(|| {
        let mut names = std::collections::BTreeSet::new();
        if let Some(path) = std::env::var_os("PATH") {
            for dir in std::env::split_paths(&path) {
                let Ok(entries) = std::fs::read_dir(&dir) else {
                    continue;
                };
                for entry in entries.flatten() {
                    if is_executable(&entry) {
                        names.insert(entry.file_name().to_string_lossy().to_string());
                    }
                }
            }
        }
        names.into_iter().collect()
    })
}

/// Whether a directory entry is an executable program. Follows symlinks (e.g.
/// Homebrew's bin directory is almost entirely symlinks).
#[cfg(unix)]
fn is_executable(entry: &std::fs::DirEntry) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(entry.path())
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(windows)]
fn is_executable(entry: &std::fs::DirEntry) -> bool {
    let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
    ["exe", "cmd", "bat", "com", "ps1"]
        .iter()
        .any(|ext| name.ends_with(&format!(".{ext}")))
}

/// Keep local image attachments bounded: base64 and session JSON add roughly
/// another third on top of the raw bytes.
const MAX_IMAGE_BYTES: u64 = 20 * 1024 * 1024;
const MAX_TOTAL_IMAGE_BYTES: u64 = 50 * 1024 * 1024;
const MAX_IMAGES_PER_MESSAGE: usize = 20;

#[derive(Debug, Default)]
pub(crate) struct ExpandedFileReferences {
    pub text: String,
    pub images: Vec<InputImageContent>,
    pub notices: Vec<String>,
}

/// Expand `@path` references in a sent message. Supported images are attached
/// when vision is enabled; every other existing file is passed to the model as
/// a path-only reference so its contents do not consume the request context.
/// Tokens that do not resolve to a readable file are left alone.
pub(crate) async fn expand_file_references(
    text: &str,
    vision_enabled: bool,
) -> ExpandedFileReferences {
    let mut seen: Vec<String> = Vec::new();
    let mut references = String::new();
    let mut images = Vec::new();
    let mut notices = Vec::new();
    let mut total_image_bytes = 0u64;

    for raw in text.split_whitespace() {
        let Some(path) = raw.strip_prefix('@') else {
            continue;
        };
        // Ignore empty and already-processed references.
        if path.is_empty() || seen.iter().any(|p| p == path) {
            continue;
        }
        let fs_path = expand_tilde(path);
        let Ok(meta) = tokio::fs::metadata(&fs_path).await else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }

        let Ok(mut file) = tokio::fs::File::open(&fs_path).await else {
            continue;
        };
        let mut header = [0u8; 12];
        let Ok(header_len) = file.read(&mut header).await else {
            continue;
        };
        seen.push(path.to_string());

        if let Some(kind) = detect_image(&header[..header_len]) {
            if !vision_enabled {
                notices.push(format!(
                    "skipped image @{path}: vision is off; use /vision on to attach images"
                ));
                continue;
            }
            if images.len() >= MAX_IMAGES_PER_MESSAGE {
                notices.push(format!(
                    "skipped image @{path}: at most {MAX_IMAGES_PER_MESSAGE} images may be attached to one message"
                ));
                continue;
            }
            if meta.len() > MAX_IMAGE_BYTES {
                notices.push(format!(
                    "skipped image @{path}: file size is {} bytes; limit is {MAX_IMAGE_BYTES}",
                    meta.len()
                ));
                continue;
            }
            if total_image_bytes.saturating_add(meta.len()) > MAX_TOTAL_IMAGE_BYTES {
                notices.push(format!(
                    "skipped image @{path}: total image size per message is limited to {MAX_TOTAL_IMAGE_BYTES} bytes"
                ));
                continue;
            }
            let Ok(bytes) = tokio::fs::read(&fs_path).await else {
                continue;
            };
            if kind == ImageKind::Gif && gif_frame_count(&bytes).unwrap_or(2) != 1 {
                notices.push(format!(
                    "skipped image @{path}: animated or malformed GIF files are not supported"
                ));
                continue;
            }
            images.push(InputImageContent {
                detail: ImageDetail::Auto,
                file_id: None,
                image_url: Some(format!(
                    "data:{};base64,{}",
                    kind.mime_type(),
                    BASE64.encode(&bytes)
                )),
            });
            total_image_bytes += meta.len();
            continue;
        }

        references.push_str(&format!(
            "\n\nReferenced local file path (content not included): {fs_path}"
        ));
    }

    ExpandedFileReferences {
        text: if references.is_empty() {
            text.to_string()
        } else {
            format!("{text}{references}")
        },
        images,
        notices,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ImageKind {
    Png,
    Jpeg,
    Webp,
    Gif,
}

impl ImageKind {
    fn mime_type(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Webp => "image/webp",
            Self::Gif => "image/gif",
        }
    }
}

fn detect_image(bytes: &[u8]) -> Option<ImageKind> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(ImageKind::Png)
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Some(ImageKind::Jpeg)
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some(ImageKind::Webp)
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some(ImageKind::Gif)
    } else {
        None
    }
}

/// Parse enough of a GIF stream to count image-descriptor blocks without
/// mistaking compressed payload bytes for frames.
fn gif_frame_count(bytes: &[u8]) -> Option<usize> {
    if detect_image(bytes) != Some(ImageKind::Gif) || bytes.len() < 13 {
        return None;
    }
    let mut pos: usize = 13;
    let packed = bytes[10];
    if packed & 0x80 != 0 {
        pos = pos.checked_add(3 * (1usize << ((packed & 0x07) + 1)))?;
    }
    let mut frames = 0;
    while pos < bytes.len() {
        match bytes[pos] {
            0x2c => {
                frames += 1;
                pos = pos.checked_add(10)?;
                let local_packed = *bytes.get(pos - 1)?;
                if local_packed & 0x80 != 0 {
                    pos = pos.checked_add(3 * (1usize << ((local_packed & 0x07) + 1)))?;
                }
                pos = pos.checked_add(1)?;
                skip_gif_sub_blocks(bytes, &mut pos)?;
            }
            0x21 => {
                pos = pos.checked_add(2)?;
                skip_gif_sub_blocks(bytes, &mut pos)?;
            }
            0x3b => return Some(frames),
            _ => return None,
        }
    }
    None
}

fn skip_gif_sub_blocks(bytes: &[u8], pos: &mut usize) -> Option<()> {
    loop {
        let size = usize::from(*bytes.get(*pos)?);
        *pos = pos.checked_add(1)?;
        if size == 0 {
            return Some(());
        }
        *pos = pos.checked_add(size)?;
        if *pos > bytes.len() {
            return None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thinking_command_is_parsed_and_off_is_not_a_level() {
        assert!(matches!(
            Command::parse("/thinking high"),
            Some(Command::Thinking(level)) if level == "high"
        ));
        assert!(Command::all_commands().any(|name| name == "thinking"));
        assert_eq!(crate::thinking::ThinkingLevel::parse("off"), None);

        let state = CompletionEngine::complete_subcommand(
            "thinking h",
            "thinking",
            crate::thinking::ThinkingLevel::COMPLETIONS,
        )
        .expect("thinking level completion");
        assert_eq!(state.candidates, vec!["high"]);
    }

    #[test]
    fn command_catalog_preserves_names_aliases_and_argument_parsing() {
        let expected_names = [
            "model",
            "new",
            "providers",
            "session",
            "mode",
            "classifier",
            "init",
            "todo",
            "skill",
            "mcp",
            "plan",
            "terminal",
            "compact",
            "thinking",
            "vision",
            "clear",
            "quit",
            "help",
        ];
        assert_eq!(Command::all_commands().collect::<Vec<_>>(), expected_names);

        for spec in COMMAND_SPECS {
            for name in std::iter::once(spec.name).chain(spec.aliases.iter().copied()) {
                let input = format!("  /{name}   test argument  ");
                assert_eq!(
                    Command::parse(&input),
                    Some(spec.kind.command("test argument".to_string())),
                    "failed to parse /{name}"
                );
            }
        }
    }

    #[test]
    fn unknown_slash_commands_still_fall_through() {
        assert_eq!(Command::parse("/not-a-programmer-command value"), None);
        assert_eq!(Command::parse("not a slash command"), None);
        assert_eq!(Command::parse("/"), None);
    }

    #[test]
    fn command_catalog_preserves_help_order_and_text() {
        let expected = vec![
            ("/model <provider/model>", "Switch to a different model"),
            (
                "/mode <manual|auto|plan|yolo>",
                "Set work mode (or cycle with Ctrl+T)",
            ),
            (
                "/classifier [provider/model]",
                "Set/show the Auto-mode classifier model",
            ),
            (
                "/init",
                "Explore the project, write PROGRAMMER.md, set up diagnostics",
            ),
            ("/plan approve", "Approve the current plan (Plan mode)"),
            ("/plan cancel", "Cancel plan and return to Auto mode"),
            (
                "/skill <name|list|off>",
                "Activate, list, or clear agent skills",
            ),
            ("/skill manage", "Open the skills management panel"),
            ("/mcp show", "List configured MCP servers and their status"),
            ("/mcp manage", "Open the MCP server management panel"),
            (
                "/terminal [id]",
                "Open the interactive terminal for a PTY task",
            ),
            (
                "/compact [provider/model]",
                "Summarize older history to shrink the model's context",
            ),
            (
                "/thinking [auto|none|minimal|low|medium|high|xhigh]",
                "Set/show reasoning effort for chat and compaction",
            ),
            (
                "/vision <on|off>",
                "Enable or disable image attachments for this session",
            ),
            ("/todo | /t", "Open the todo list panel"),
            ("/new | /n", "Start a new session (saves current)"),
            (
                "/providers show",
                "List all configured providers and models",
            ),
            ("/providers manage", "Open the provider management panel"),
            ("/session | /s", "Show current session info"),
            ("/clear | /c", "Clear the conversation history"),
            ("/quit | /q", "Exit the application"),
            ("/help | /?", "Show this help"),
        ];
        assert_eq!(Command::descriptions(), expected);
    }

    #[test]
    fn command_catalog_has_unique_names_aliases_and_help_order() {
        let mut names = std::collections::HashSet::new();
        let mut help_orders = std::collections::HashSet::new();

        for spec in COMMAND_SPECS {
            assert!(
                names.insert(spec.name),
                "duplicate command name: {}",
                spec.name
            );
            for alias in spec.aliases {
                assert!(names.insert(*alias), "duplicate command alias: {alias}");
            }
            assert!(!spec.help.is_empty(), "{} has no help entry", spec.name);
            for entry in spec.help {
                assert!(
                    help_orders.insert(entry.order),
                    "duplicate help order: {}",
                    entry.order
                );
            }
        }

        assert_eq!(help_orders.len(), Command::descriptions().len());
        assert_eq!(
            help_orders.iter().copied().max().map(usize::from),
            Some(help_orders.len() - 1)
        );
    }

    #[test]
    fn catalog_drives_fixed_and_alias_completions() {
        let providers = COMMAND_SPECS
            .iter()
            .find(|spec| spec.name == "providers")
            .expect("providers spec");
        assert!(providers.matches("provider"));
        let CompletionKind::Fixed(values) = providers.completion else {
            panic!("providers should use fixed completions");
        };
        let state = CompletionEngine::complete_subcommand("provider m", "provider", values)
            .expect("provider alias completion");
        assert_eq!(state.prefix, "/provider ");
        assert_eq!(state.candidates, vec!["manage"]);

        let mode = COMMAND_SPECS
            .iter()
            .find(|spec| spec.name == "mode")
            .expect("mode spec");
        let CompletionKind::Fixed(values) = mode.completion else {
            panic!("mode should use fixed completions");
        };
        assert_eq!(values, &["manual", "auto", "plan", "yolo"]);
        let state = CompletionEngine::complete_subcommand("mode p", "mode", values)
            .expect("mode plan completion");
        assert_eq!(state.candidates, vec!["plan"]);

        let plan = COMMAND_SPECS
            .iter()
            .find(|spec| spec.name == "plan")
            .expect("plan spec");
        let CompletionKind::Fixed(values) = plan.completion else {
            panic!("plan should use fixed completions");
        };
        assert_eq!(values, &["approve", "cancel"]);
        let state = CompletionEngine::complete_subcommand("plan c", "plan", values)
            .expect("plan cancel completion");
        assert_eq!(state.candidates, vec!["cancel"]);
    }

    #[test]
    fn active_at_token_detects_trailing_reference() {
        let (prefix, partial) = active_at_token("explain @src/con").unwrap();
        assert_eq!(prefix, "explain @");
        assert_eq!(partial, "src/con");
    }

    #[test]
    fn active_at_token_ignores_non_reference_tokens() {
        assert!(active_at_token("just some text").is_none());
        assert!(active_at_token("email me a@b.com now").is_none());
    }

    #[test]
    fn active_at_token_handles_bare_at() {
        let (prefix, partial) = active_at_token("look at @").unwrap();
        assert_eq!(prefix, "look at @");
        assert_eq!(partial, "");
    }

    #[cfg(unix)]
    #[test]
    fn bang_completion_lists_path_commands_and_files() {
        // `ls` exists on every Unix box; the exact name sorts first among the
        // `ls*` matches, so the 50-candidate cap can't push it out.
        let state = CompletionEngine::complete_bang("!ls").expect("commands starting with ls");
        assert_eq!(
            state.candidates.first().map(String::as_str),
            Some("ls"),
            "{:?}",
            state.candidates
        );
        assert_eq!(state.prefix, "!");
        assert_eq!(state.line(0), "!ls");

        // A bare `!` completes nothing.
        assert!(CompletionEngine::complete_bang("!").is_none());

        // Arguments complete as paths (runs from the crate root).
        let state = CompletionEngine::complete_bang("!cat src/co").expect("path candidates");
        assert_eq!(state.prefix, "!cat ");
        assert!(
            state.candidates.iter().any(|c| c == "src/commands/"),
            "{:?}",
            state.candidates
        );

        // A first word containing `/` completes as a path too.
        let state = CompletionEngine::complete_bang("!./src/mai");
        assert!(state.is_none() || state.unwrap().prefix == "!");
    }

    #[cfg(unix)]
    #[test]
    fn tilde_paths_expand_but_stay_tilde_in_candidates() {
        let home = std::env::var("HOME").expect("HOME is set on unix");
        assert_eq!(expand_tilde("~/xinbot/"), format!("{home}/xinbot/"));
        assert_eq!(expand_tilde("plain/path"), "plain/path");

        // A bare `~` completes to the home directory itself.
        assert_eq!(list_path_candidates("~"), vec!["~/".to_string()]);
        // Candidates under `~/` keep the tilde form the user typed.
        assert!(
            list_path_candidates("~/")
                .iter()
                .all(|c| c.starts_with("~/"))
        );
    }

    #[test]
    fn list_path_candidates_reads_one_level() {
        // Runs from the crate root, so `src/` exists with these entries.
        let got = list_path_candidates("src/co");
        assert!(
            got.iter().any(|c| c == "src/commands/"),
            "dir with slash: {got:?}"
        );
        assert!(got.iter().any(|c| c == "src/consts.rs"), "file: {got:?}");
        // Directories sort before files.
        let dir_pos = got.iter().position(|c| c == "src/commands/").unwrap();
        let file_pos = got.iter().position(|c| c == "src/consts.rs").unwrap();
        assert!(dir_pos < file_pos, "dirs first: {got:?}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn terminal_completion_lists_all_running_tasks() {
        let interactive_id =
            crate::tasks::spawn_interactive("cat", None, Some("catname"), 10, 40).expect("spawn");
        let pipe_id = crate::tasks::spawn("sleep 5", None, Some("sleep")).expect("spawn");
        let state = CompletionEngine::complete_terminal("terminal ", "terminal")
            .expect("candidates for running tasks");
        for id in [interactive_id, pipe_id] {
            assert!(
                state
                    .candidates
                    .iter()
                    .any(|c| c.starts_with(&format!("{id}  "))),
                "candidates: {:?}",
                state.candidates
            );
            crate::tasks::kill(id).ok();
        }
    }

    #[tokio::test]
    async fn expand_file_references_adds_path_without_content() {
        let out = expand_file_references("look at @Cargo.toml please", false).await;
        assert!(
            out.text.starts_with("look at @Cargo.toml please"),
            "keeps typed text"
        );
        assert!(
            out.text
                .contains("Referenced local file path (content not included): Cargo.toml"),
            "has path reference"
        );
        assert!(
            !out.text.contains("[package]"),
            "does not inline file content"
        );
        assert!(out.images.is_empty());
    }

    #[tokio::test]
    async fn expand_file_references_leaves_plain_text_alone() {
        let out = expand_file_references("no references here @nonexistent.xyz", false).await;
        assert_eq!(out.text, "no references here @nonexistent.xyz");
        assert!(out.images.is_empty());
        assert!(out.notices.is_empty());
    }

    #[tokio::test]
    async fn binary_file_reference_adds_only_its_path() {
        let path =
            std::env::temp_dir().join(format!("programmer_binary_{}.bin", std::process::id()));
        tokio::fs::write(&path, [0, 0xff, 0xfe, 0xfd])
            .await
            .unwrap();
        let reference = format!("@{}", path.display());

        let out = expand_file_references(&reference, false).await;
        assert!(out.text.contains(&format!(
            "Referenced local file path (content not included): {}",
            path.display()
        )));
        assert!(out.images.is_empty());
        assert!(out.notices.is_empty());
        tokio::fs::remove_file(path).await.ok();
    }

    #[tokio::test]
    async fn image_reference_respects_vision_switch() {
        let path =
            std::env::temp_dir().join(format!("programmer_vision_{}.png", std::process::id()));
        tokio::fs::write(&path, b"\x89PNG\r\n\x1a\nfake")
            .await
            .unwrap();
        let reference = format!("@{}", path.display());

        let off = expand_file_references(&reference, false).await;
        assert!(off.images.is_empty());
        assert!(off.notices.iter().any(|n| n.contains("vision is off")));

        let on = expand_file_references(&reference, true).await;
        assert_eq!(on.images.len(), 1);
        assert!(
            on.images[0]
                .image_url
                .as_deref()
                .is_some_and(|url| url.starts_with("data:image/png;base64,"))
        );
        tokio::fs::remove_file(path).await.ok();
    }

    #[test]
    fn gif_parser_distinguishes_single_frame_from_animation() {
        let single = b"GIF89a\x01\0\x01\0\0\0\0,\0\0\0\0\x01\0\x01\0\0\x02\x01\0\0;";
        let animated = b"GIF89a\x01\0\x01\0\0\0\0,\0\0\0\0\x01\0\x01\0\0\x02\x01\0\0,\0\0\0\0\x01\0\x01\0\0\x02\x01\0\0;";
        assert_eq!(gif_frame_count(single), Some(1));
        assert_eq!(gif_frame_count(animated), Some(2));
    }
}
