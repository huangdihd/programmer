// Copyright (C) 2026 huangdihd
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

//! Command-line interface definitions. Runtime behavior lives in `main` and
//! [`crate::headless`], keeping parsing independently testable.

use clap::{Args as ClapArgs, CommandFactory, Parser, Subcommand, ValueEnum};
use std::net::SocketAddr;
use std::path::PathBuf;

use crate::classifier::WorkMode;
use crate::thinking::ThinkingLevel;

#[derive(Debug, Parser)]
#[command(name = "programmer", version, about = "A coding agent written in Rust")]
pub(crate) struct Args {
    /// Resume a saved session; without a UUID, open the session picker.
    #[arg(
        long,
        value_name = "UUID",
        num_args = 0..=1,
        default_missing_value = "",
        conflicts_with = "session"
    )]
    pub resume: Option<String>,

    /// Open the session management panel on startup.
    #[arg(long)]
    pub session: bool,

    /// Open the provider management panel on startup.
    #[arg(long)]
    pub providers: bool,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    /// Run the agent without the terminal UI.
    Run(RunArgs),
    /// Initialize PROGRAMMER.md and project diagnostics with the agent.
    Init(InitArgs),
    /// Run configured project diagnostics without calling a model.
    Diagnostics(DiagnosticsArgs),
    /// Expose programmer's local tools over MCP.
    Mcp(McpArgs),
}

#[derive(Debug, ClapArgs)]
#[command(group(
    clap::ArgGroup::new("prompt_source")
        .args(["prompt", "prompt_file"])
        .required(true)
))]
pub(crate) struct RunArgs {
    /// Prompt text. Use `-` to read it from stdin.
    #[arg(value_name = "PROMPT")]
    pub prompt: Option<String>,

    /// Read the prompt from a UTF-8 file.
    #[arg(long, value_name = "PATH")]
    pub prompt_file: Option<PathBuf>,

    /// Run the hidden project initialization turn before the user prompt.
    #[arg(long)]
    pub init: bool,

    /// Run one final diagnostics snapshot after the agent finishes.
    #[arg(long)]
    pub check: bool,

    /// Make the final check fail on this severity or anything more severe.
    #[arg(long, value_enum, default_value_t = DiagnosticThreshold::Error, requires = "check")]
    pub fail_on: DiagnosticThreshold,

    /// Output the result as human-readable text, one JSON document, or JSONL events.
    #[arg(long, value_enum, default_value_t = AgentOutputFormat::Text)]
    pub format: AgentOutputFormat,

    #[command(flatten)]
    pub agent: AgentArgs,
}

#[derive(Debug, ClapArgs)]
pub(crate) struct InitArgs {
    /// Output the result as human-readable text, one JSON document, or JSONL events.
    #[arg(long, value_enum, default_value_t = AgentOutputFormat::Text)]
    pub format: AgentOutputFormat,

    #[command(flatten)]
    pub agent: AgentArgs,
}

#[derive(Debug, Clone, ClapArgs)]
pub(crate) struct AgentArgs {
    /// Chat model as `provider/model`; a bare model uses the default provider.
    #[arg(long, value_name = "MODEL")]
    pub model: Option<String>,

    /// Classifier model used in Auto mode.
    #[arg(long, value_name = "MODEL")]
    pub classifier_model: Option<String>,

    /// Reasoning effort sent to the chat model.
    #[arg(long, value_enum, default_value_t = ThinkingLevel::Auto)]
    pub thinking: ThinkingLevel,

    /// Tool-gating mode. Manual is unavailable without an interactive surface.
    #[arg(long, value_enum, default_value_t = WorkMode::Auto)]
    pub work_mode: WorkMode,

    /// Disable automatic diagnostics after successful file edits.
    #[arg(long)]
    pub no_diagnostics: bool,

    /// Run from this project directory.
    #[arg(long, value_name = "PATH")]
    pub cwd: Option<PathBuf>,

    /// Cancel the complete headless operation after this many seconds.
    #[arg(long, value_name = "SECONDS", value_parser = clap::value_parser!(u64).range(1..))]
    pub timeout: Option<u64>,

    /// Stop after this many model responses, including tool-call responses.
    #[arg(long, value_name = "COUNT", value_parser = parse_positive_usize)]
    pub max_steps: Option<usize>,
}

fn parse_positive_usize(value: &str) -> Result<usize, String> {
    let parsed = value
        .parse::<usize>()
        .map_err(|_| "must be a positive integer".to_string())?;
    if parsed == 0 {
        return Err("must be greater than zero".to_string());
    }
    Ok(parsed)
}

#[derive(Debug, ClapArgs)]
pub(crate) struct DiagnosticsArgs {
    /// Output a human-readable report or a stable JSON document.
    #[arg(long, value_enum, default_value_t = DiagnosticsOutputFormat::Text)]
    pub format: DiagnosticsOutputFormat,

    /// Exit unsuccessfully on this severity or anything more severe.
    #[arg(long, value_enum, default_value_t = DiagnosticThreshold::Error)]
    pub fail_on: DiagnosticThreshold,

    /// Run from this project directory.
    #[arg(long, value_name = "PATH")]
    pub cwd: Option<PathBuf>,
}

#[derive(Debug, ClapArgs)]
pub(crate) struct McpArgs {
    #[command(subcommand)]
    pub command: McpCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum McpCommand {
    /// Serve newline-delimited JSON-RPC over stdin/stdout.
    Stdio(McpStdioArgs),
    /// Serve HTTP JSON-RPC with an interactive approval console.
    Http(McpHttpArgs),
}

#[derive(Debug, ClapArgs)]
pub(crate) struct McpStdioArgs {
    /// Non-interactive tool-gating mode (Auto or YOLO).
    #[arg(long, value_enum, default_value_t = WorkMode::Auto)]
    pub work_mode: WorkMode,

    /// Run from this project directory.
    #[arg(long, value_name = "PATH")]
    pub cwd: Option<PathBuf>,
}

#[derive(Debug, ClapArgs)]
pub(crate) struct McpHttpArgs {
    /// Address for the HTTP MCP server.
    #[arg(value_name = "ADDR", default_value = "127.0.0.1:8765")]
    pub addr: SocketAddr,

    /// Tool-gating mode controlled by the approval console.
    #[arg(long, value_enum, default_value_t = WorkMode::Auto)]
    pub work_mode: WorkMode,

    /// Run from this project directory.
    #[arg(long, value_name = "PATH")]
    pub cwd: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum AgentOutputFormat {
    Text,
    Json,
    Jsonl,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum DiagnosticsOutputFormat {
    Text,
    Json,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum DiagnosticThreshold {
    Error,
    Warning,
    Lint,
}

impl DiagnosticThreshold {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
            Self::Lint => "lint",
        }
    }

    pub(crate) fn matches(self, severity: crate::diagnostics::Severity) -> bool {
        use crate::diagnostics::Severity;
        match self {
            Self::Error => severity == Severity::Error,
            Self::Warning => matches!(severity, Severity::Error | Severity::Warning),
            Self::Lint => severity != Severity::Info,
        }
    }
}

impl Args {
    pub(crate) fn parse_and_validate() -> Self {
        Self::try_parse()
            .and_then(Self::validate)
            .unwrap_or_else(|error| error.exit())
    }

    pub(crate) fn validate(self) -> Result<Self, clap::Error> {
        if self.command.is_some() && (self.resume.is_some() || self.session || self.providers) {
            return Err(self.usage_error(
                "TUI flags --resume, --session, and --providers cannot be used with a subcommand",
            ));
        }

        match &self.command {
            Some(Command::Run(args)) => {
                if args.agent.work_mode == WorkMode::Manual {
                    return Err(self.usage_error(
                        "programmer run is non-interactive and does not support --work-mode manual",
                    ));
                }
                if args.init && args.agent.work_mode == WorkMode::Plan {
                    return Err(self.usage_error(
                        "programmer run --init cannot initialize files in --work-mode plan",
                    ));
                }
            }
            Some(Command::Init(args)) => {
                if matches!(args.agent.work_mode, WorkMode::Manual | WorkMode::Plan) {
                    return Err(
                        self.usage_error("programmer init requires --work-mode auto or yolo")
                    );
                }
            }
            Some(Command::Mcp(McpArgs {
                command: McpCommand::Stdio(args),
            })) if !matches!(args.work_mode, WorkMode::Auto | WorkMode::Yolo) => {
                return Err(
                    self.usage_error("programmer mcp stdio requires --work-mode auto or yolo")
                );
            }
            _ => {}
        }

        Ok(self)
    }

    fn usage_error(&self, message: impl Into<String>) -> clap::Error {
        let mut command = Self::command();
        clap::Error::raw(clap::error::ErrorKind::ArgumentConflict, message.into())
            .format(&mut command)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Args, clap::Error> {
        Args::try_parse_from(args).and_then(Args::validate)
    }

    #[test]
    fn bare_command_starts_the_tui() {
        let args = parse(&["programmer"]).unwrap();
        assert!(args.command.is_none());
    }

    #[test]
    fn run_accepts_headless_controls() {
        let args = parse(&[
            "programmer",
            "run",
            "fix it",
            "--model",
            "openai/gpt-test",
            "--thinking",
            "high",
            "--max-steps",
            "8",
            "--check",
            "--fail-on",
            "warning",
        ])
        .unwrap();
        let Some(Command::Run(run)) = args.command else {
            panic!("expected run command");
        };
        assert_eq!(run.agent.model.as_deref(), Some("openai/gpt-test"));
        assert_eq!(run.agent.thinking, ThinkingLevel::High);
        assert_eq!(run.agent.max_steps, Some(8));
        assert_eq!(run.fail_on, DiagnosticThreshold::Warning);
    }

    #[test]
    fn run_uses_defaults_without_requiring_a_final_check() {
        let args = parse(&["programmer", "run", "hello"]).unwrap();
        let Some(Command::Run(run)) = args.command else {
            panic!("expected run command");
        };
        assert!(!run.check);
        assert_eq!(run.fail_on, DiagnosticThreshold::Error);
    }

    #[test]
    fn run_requires_exactly_one_prompt_source() {
        assert!(parse(&["programmer", "run"]).is_err());
        assert!(parse(&["programmer", "run", "inline", "--prompt-file", "prompt.txt"]).is_err());
    }

    #[test]
    fn non_interactive_modes_are_validated() {
        assert!(parse(&["programmer", "run", "hello", "--work-mode", "manual"]).is_err());
        assert!(
            parse(&[
                "programmer",
                "run",
                "hello",
                "--init",
                "--work-mode",
                "plan"
            ])
            .is_err()
        );
        assert!(parse(&["programmer", "mcp", "stdio", "--work-mode", "manual"]).is_err());
        assert!(parse(&["programmer", "mcp", "http", "--work-mode", "manual"]).is_ok());
    }

    #[test]
    fn old_flat_headless_flags_are_removed() {
        assert!(Args::try_parse_from(["programmer", "--print", "hello"]).is_err());
        assert!(Args::try_parse_from(["programmer", "--mcp-server"]).is_err());
        assert!(Args::try_parse_from(["programmer", "--mcp-http"]).is_err());
    }

    #[test]
    fn tui_flags_conflict_with_subcommands() {
        assert!(parse(&["programmer", "--providers", "diagnostics"]).is_err());
    }
}
