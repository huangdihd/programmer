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

// ---------------------------------------------------------------------------
// Command parsing
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum Command {
    Quit,
    Clear,
    New,
    Model(String),
    /// `/providers <subcommand>` — carries the raw argument string
    /// ("show", "manage", or anything else for the usage hint).
    Providers(String),
    /// `/mode <manual|edits|auto|yolo>` — cycle/set work mode.
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
}

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

        match cmd {
            "q" | "quit" | "exit" => Some(Command::Quit),
            "c" | "clear" => Some(Command::Clear),
            "new" | "n" => Some(Command::New),
            "model" | "m" => Some(Command::Model(args)),
            "providers" | "provider" => Some(Command::Providers(args)),
            "mode" => Some(Command::Mode(args)),
            "classifier" => Some(Command::Classifier(args)),
            "init" => Some(Command::Init),
            "help" | "?" => Some(Command::Help),
            "session" | "s" => Some(Command::Session),
            "todo" | "todos" | "t" => Some(Command::Todo),
            "skill" | "skills" => Some(Command::Skill(args)),
            "mcp" => Some(Command::Mcp(args)),
            "plan" => Some(Command::Plan(args)),
            _ => None,
        }
    }

    /// All command names (without leading `/`), for completion.
    pub fn all_commands() -> &'static [&'static str] {
        &[
            "model", "new", "providers", "session", "mode", "classifier", "init", "todo", "skill",
            "mcp", "plan", "clear", "quit", "help",
        ]
    }

    /// Human-readable descriptions for the help text.
    pub fn descriptions() -> &'static [(&'static str, &'static str)] {
        &[
            ("/model <provider/model>", "Switch to a different model"),
            ("/mode <manual|auto|plan|yolo>", "Set work mode (or cycle with Ctrl+T)"),
            ("/classifier [provider/model]", "Set/show the Auto-mode classifier model"),
            ("/init", "Explore the project, write PROGRAMMER.md, set up diagnostics"),
            ("/plan approve", "Approve the current plan (Plan mode)"),
            ("/plan cancel", "Cancel plan and return to Auto mode"),
            ("/skill <name|list|off>", "Activate, list, or clear agent skills"),
            ("/skill manage", "Open the skills management panel"),
            ("/mcp show", "List configured MCP servers and their status"),
            ("/mcp manage", "Open the MCP server management panel"),
            ("/todo | /t", "Open the todo list panel"),
            ("/new | /n", "Start a new session (saves current)"),
            ("/providers show", "List all configured providers and models"),
            ("/providers manage", "Open the provider management panel"),
            ("/session | /s", "Show current session info"),
            ("/clear | /c", "Clear the conversation history"),
            ("/quit | /q", "Exit the application"),
            ("/help | /?", "Show this help"),
        ]
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
                .iter()
                .filter(|c| c.starts_with(typed))
                .map(|c| format!("/{}", c))
                .collect();
            return CompletionState::new(String::new(), candidates);
        }

        let cmd = parts[0];
        match cmd {
            "model" | "m" => Self::complete_model(text, cmd, pm),
            "classifier" => Self::complete_model(text, cmd, pm),
            "mode" => Self::complete_subcommand(text, cmd, &["manual", "edits", "auto"]),
            "providers" | "provider" => Self::complete_subcommand(text, cmd, &["show", "manage"]),
            "skill" | "skills" => Self::complete_skill(text, cmd, skill_registry),
            "mcp" => Self::complete_subcommand(text, cmd, &["show", "manage"]),
            _ => None,
        }
    }

    /// Complete a fixed set of subcommands for `cmd`.
    fn complete_subcommand(
        text: &str,
        cmd: &str,
        subcommands: &[&str],
    ) -> Option<CompletionState> {
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
