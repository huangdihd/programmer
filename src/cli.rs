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
#[allow(clippy::large_enum_variant)]
pub(crate) enum Command {
    /// Run the agent without the terminal UI.
    Run(RunArgs),
    /// Initialize PROGRAMMER.md and project diagnostics with the agent.
    Init(InitArgs),
    /// Run configured project diagnostics without calling a model.
    Diagnostics(DiagnosticsArgs),
    /// Expose programmer's local tools over MCP.
    Mcp(McpArgs),
    /// Update programmer to the latest release.
    Upgrade(UpgradeArgs),
    /// Remove programmer from this machine.
    Uninstall(UninstallArgs),
}

#[derive(Debug, ClapArgs)]
#[command(group(
    clap::ArgGroup::new("prompt_source")
        .args(["prompt", "prompt_file"])
))]
#[command(group(
    clap::ArgGroup::new("session_source")
        .args(["resume", "continue_session"])
))]
#[command(group(
    clap::ArgGroup::new("answer_source")
        .args(["answer", "answer_file"])
        .requires("session_source")
))]
pub(crate) struct RunArgs {
    /// Prompt text. Use `-` to read it from stdin.
    #[arg(value_name = "PROMPT", required_unless_present_any = ["prompt_file", "answer_source", "session_source"])]
    pub prompt: Option<String>,

    /// Read the prompt from a UTF-8 file.
    #[arg(long, value_name = "PATH")]
    pub prompt_file: Option<PathBuf>,

    /// Suspend when user input or approval is needed, saving the session for later.
    #[arg(long, conflicts_with = "no_save")]
    pub suspend: bool,

    /// Answer a pending question or approval in a resumed session.
    #[arg(long, value_name = "TEXT")]
    pub answer: Option<String>,

    /// Read an answer from a UTF-8 file.
    #[arg(long, value_name = "PATH")]
    pub answer_file: Option<PathBuf>,

    /// Identify the pending question or approval being answered.
    #[arg(long, value_name = "ID", requires = "answer_source")]
    pub answer_id: Option<String>,

    /// Append text to the prompt (repeatable); all text is appended before files.
    #[arg(long, value_name = "TEXT")]
    pub append_prompt: Vec<String>,

    /// Append UTF-8 file contents (repeatable), after all --append-prompt text.
    #[arg(long, value_name = "PATH")]
    pub append_prompt_file: Vec<PathBuf>,

    /// Attach an image to the user prompt (repeatable).
    #[arg(long, value_name = "PATH", conflicts_with = "no_vision")]
    pub image: Vec<PathBuf>,

    /// Save this new conversation as a persistent session.
    #[arg(long, conflicts_with = "no_save")]
    pub save_session: bool,

    /// Resume a saved session by UUID (no interactive picker).
    #[arg(long, value_name = "UUID", value_parser = parse_session_uuid, conflicts_with = "continue_session")]
    pub resume: Option<String>,

    /// Continue the most recently updated session in the current project.
    #[arg(long = "continue")]
    pub continue_session: bool,

    /// Continue in a new session instead of overwriting the original.
    #[arg(long, requires = "session_source")]
    pub fork: bool,

    /// Do not save changes to the resumed conversation.
    #[arg(long, requires = "session_source", conflicts_with = "fork")]
    pub no_save: bool,

    /// Run the hidden project initialization turn before the user prompt.
    #[arg(long, conflicts_with_all = ["resume", "continue_session"])]
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

/// Session-only security overrides shared by every non-TUI execution path.
#[derive(Debug, Clone, Default, ClapArgs)]
pub(crate) struct SecurityArgs {
    /// Select a configured security profile for this invocation only.
    #[arg(long, value_name = "NAME")]
    pub security_profile: Option<String>,

    /// Process sandbox mode; does not disable filesystem policy.
    #[arg(long, value_parser = ["restricted", "network", "off"])]
    pub sandbox: Option<String>,

    /// Additional process-readable path (repeatable, relative to --cwd).
    #[arg(long, value_name = "PATH")]
    pub allow_read: Vec<PathBuf>,

    /// Additional tool/process-writable path (repeatable, relative to --cwd).
    #[arg(long, value_name = "PATH")]
    pub allow_write: Vec<PathBuf>,

    /// Deny tool/process reads of this path (repeatable, relative to --cwd).
    #[arg(long, value_name = "PATH")]
    pub deny_read: Vec<PathBuf>,

    /// Remove matching environment variables from subprocesses (repeatable).
    #[arg(long, value_name = "GLOB")]
    pub deny_env: Vec<String>,

    /// Disable read-before-write and change protection, not filesystem policy.
    #[arg(long)]
    pub no_protect_file_changes: bool,
}

impl SecurityArgs {
    pub(crate) fn has_overrides(&self) -> bool {
        self.security_profile.is_some()
            || self.sandbox.is_some()
            || self.no_protect_file_changes
            || !self.allow_read.is_empty()
            || !self.allow_write.is_empty()
            || !self.deny_read.is_empty()
            || !self.deny_env.is_empty()
    }
}

#[derive(Debug, Clone, ClapArgs)]
pub(crate) struct AgentArgs {
    #[command(flatten)]
    pub security: SecurityArgs,

    /// Load this config file read-only (never created or rewritten); environment overrides still apply.
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,

    /// Enable image input for this invocation.
    #[arg(long, conflicts_with = "no_vision")]
    pub vision: bool,

    /// Disable image input for this invocation.
    #[arg(long)]
    pub no_vision: bool,

    /// Disable persistent memory for this invocation.
    #[arg(long)]
    pub no_memory: bool,

    /// Enable a configured MCP server by name (repeatable).
    #[arg(long, value_name = "NAME", conflicts_with = "no_mcp")]
    pub mcp: Vec<String>,

    /// Disable all MCP client servers for this invocation.
    #[arg(long)]
    pub no_mcp: bool,

    /// Allow only these tools (comma-separated, repeatable).
    #[arg(long, value_name = "TOOLS", value_delimiter = ',')]
    pub allowed_tools: Vec<String>,

    /// Disable these tools (comma-separated, repeatable); denial takes precedence.
    #[arg(long, value_name = "TOOLS", value_delimiter = ',')]
    pub disallowed_tools: Vec<String>,

    /// Override the git co-author identity for this invocation.
    #[arg(long, value_name = "IDENTITY", conflicts_with = "no_coauthor")]
    pub coauthor: Option<String>,

    /// Disable the git co-author trailer for this invocation.
    #[arg(long)]
    pub no_coauthor: bool,

    /// Load the agent personality from a UTF-8 file.
    #[arg(long, value_name = "PATH")]
    pub soul_file: Option<PathBuf>,

    /// Chat model as `provider/model`; a bare model uses the default provider.
    #[arg(long, value_name = "MODEL")]
    pub model: Option<String>,

    /// Classifier model used in Auto mode.
    #[arg(long, value_name = "MODEL")]
    pub classifier_model: Option<String>,

    /// Reasoning effort sent to the chat model.
    #[arg(long, value_enum)]
    pub thinking: Option<ThinkingLevel>,

    /// Tool-gating mode. Manual is supported by run, but not init.
    #[arg(long, value_enum)]
    pub work_mode: Option<WorkMode>,

    /// Override automatic context compaction threshold in tokens (0 disables it).
    #[arg(long, value_name = "TOKENS")]
    pub compact_tokens: Option<u32>,

    /// Override mandatory context compaction threshold in tokens (0 disables it).
    #[arg(long, value_name = "TOKENS")]
    pub mandatory_compact_tokens: Option<u32>,

    /// Disable automatic and mandatory context compaction for this invocation.
    #[arg(long, conflicts_with_all = ["compact_tokens", "mandatory_compact_tokens"])]
    pub no_compact: bool,

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

fn parse_session_uuid(value: &str) -> Result<String, String> {
    uuid::Uuid::parse_str(value)
        .map(|uuid| uuid.to_string())
        .map_err(|_| "must be a valid session UUID".to_string())
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
    /// Load this config file read-only (never created or rewritten); environment overrides still apply.
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,

    #[command(flatten)]
    pub security: SecurityArgs,

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

/// Check for and install a newer release of programmer.
#[derive(Debug, Default, ClapArgs)]
pub(crate) struct UpgradeArgs {
    /// Check for a newer version without installing it.
    #[arg(long)]
    pub check: bool,

    /// Install this specific release tag instead of the latest one.
    #[arg(long, value_name = "TAG")]
    pub tag: Option<String>,
}

/// Remove programmer from this machine.
#[derive(Debug, Default, ClapArgs)]
pub(crate) struct UninstallArgs {
    /// Also delete the configuration directory (~/.config/programmer).
    #[arg(long)]
    pub purge: bool,
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
    /// Load this config file read-only (never created or rewritten); environment overrides still apply.
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,

    #[command(flatten)]
    pub security: SecurityArgs,

    /// Non-interactive tool-gating mode (Auto or YOLO).
    #[arg(long, value_enum, default_value_t = WorkMode::Auto)]
    pub work_mode: WorkMode,

    /// Run from this project directory.
    #[arg(long, value_name = "PATH")]
    pub cwd: Option<PathBuf>,
}

#[derive(Debug, ClapArgs)]
pub(crate) struct McpHttpArgs {
    /// Load this config file read-only (never created or rewritten); environment overrides still apply.
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,

    #[command(flatten)]
    pub security: SecurityArgs,

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
                if args.agent.work_mode == Some(WorkMode::Manual) && args.no_save {
                    return Err(self.usage_error(
                        "programmer run --work-mode manual conflicts with --no-save",
                    ));
                }
                if args.init && args.agent.work_mode == Some(WorkMode::Plan) {
                    return Err(self.usage_error(
                        "programmer run --init cannot initialize files in --work-mode plan",
                    ));
                }
            }
            Some(Command::Init(args)) => {
                if matches!(
                    args.agent.work_mode,
                    Some(WorkMode::Manual | WorkMode::Plan)
                ) {
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
    fn compaction_controls_are_shared_by_run_and_init() {
        for command in [
            vec!["programmer", "run", "hello"],
            vec!["programmer", "init"],
        ] {
            for (flags, automatic, mandatory, disabled) in [
                (vec![], None, None, false),
                (vec!["--compact-tokens", "42"], Some(42), None, false),
                (
                    vec!["--mandatory-compact-tokens", "84"],
                    None,
                    Some(84),
                    false,
                ),
                (
                    vec!["--compact-tokens", "0", "--mandatory-compact-tokens", "0"],
                    Some(0),
                    Some(0),
                    false,
                ),
                (vec!["--no-compact"], None, None, true),
            ] {
                let mut argv = command.clone();
                argv.extend(flags);
                let agent = match parse(&argv).unwrap().command.unwrap() {
                    Command::Run(args) => args.agent,
                    Command::Init(args) => args.agent,
                    _ => unreachable!(),
                };
                assert_eq!(agent.compact_tokens, automatic);
                assert_eq!(agent.mandatory_compact_tokens, mandatory);
                assert_eq!(agent.no_compact, disabled);
            }
            for flag in ["--compact-tokens", "--mandatory-compact-tokens"] {
                for value in ["0", "42"] {
                    let mut argv = command.clone();
                    argv.extend([flag, value, "--no-compact"]);
                    assert_eq!(
                        parse(&argv).unwrap_err().kind(),
                        clap::error::ErrorKind::ArgumentConflict
                    );
                }
                for value in ["-1", "4294967296", "invalid"] {
                    let mut argv = command.clone();
                    argv.extend([flag, value]);
                    assert!(parse(&argv).is_err());
                }
                let mut argv = command.clone();
                argv.push(flag);
                assert!(parse(&argv).is_err());
            }
        }
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
        assert_eq!(run.agent.thinking, Some(ThinkingLevel::High));
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
    fn session_controls_preserve_explicit_overrides() {
        const UUID: &str = "12345678-1234-4234-8234-123456789abc";
        let Some(Command::Run(run)) = parse(&["programmer", "run", "hello"]).unwrap().command
        else {
            unreachable!()
        };
        assert!(run.resume.is_none());
        assert!(!run.save_session && !run.continue_session && !run.fork && !run.no_save);
        assert_eq!(run.agent.thinking, None);
        assert_eq!(run.agent.work_mode, None);

        for source in [vec!["--resume", UUID], vec!["--continue"]] {
            for extra in [
                vec![],
                vec!["--fork"],
                vec!["--no-save"],
                vec!["--save-session"],
            ] {
                let mut argv = vec!["programmer", "run", "hello"];
                argv.extend(&source);
                argv.extend(&extra);
                let Some(Command::Run(run)) = parse(&argv).unwrap().command else {
                    unreachable!()
                };
                assert_eq!(
                    run.resume.as_deref(),
                    (source[0] == "--resume").then_some(UUID)
                );
                assert_eq!(run.continue_session, source[0] == "--continue");
                assert_eq!(run.fork, extra.contains(&"--fork"));
                assert_eq!(run.no_save, extra.contains(&"--no-save"));
                assert_eq!(run.save_session, extra.contains(&"--save-session"));
            }
        }
        let Some(Command::Init(init)) = parse(&["programmer", "init"]).unwrap().command else {
            unreachable!()
        };
        assert_eq!(init.agent.thinking, None);
        assert_eq!(init.agent.work_mode, None);
        let Some(Command::Run(run)) = parse(&[
            "programmer",
            "run",
            "hello",
            "--save-session",
            "--thinking",
            "auto",
            "--work-mode",
            "auto",
        ])
        .unwrap()
        .command
        else {
            unreachable!()
        };
        assert!(run.save_session);
        assert_eq!(run.agent.thinking, Some(ThinkingLevel::Auto));
        assert_eq!(run.agent.work_mode, Some(WorkMode::Auto));
    }

    #[test]
    fn invalid_session_controls_are_usage_errors() {
        const UUID: &str = "12345678-1234-4234-8234-123456789abc";
        for flags in [
            vec!["--resume"],
            vec!["--resume", "not-a-uuid"],
            vec!["--resume", ""],
            vec!["--resume", UUID, "--continue"],
            vec!["--fork"],
            vec!["--no-save"],
            vec!["--save-session", "--no-save", "--continue"],
            vec!["--fork", "--no-save", "--continue"],
            vec!["--init", "--continue"],
            vec!["--init", "--resume", UUID],
        ] {
            let mut argv = vec!["programmer", "run", "hello"];
            argv.extend(flags);
            assert_eq!(parse(&argv).unwrap_err().exit_code(), 2, "{argv:?}");
        }
        // Runtime must inspect the saved interaction before reporting a missing prompt.
        assert!(parse(&["programmer", "run", "--continue"]).is_ok());
        for flag in ["--save-session", "--continue", "--fork", "--no-save"] {
            assert!(parse(&["programmer", "init", flag]).is_err());
        }
        assert!(parse(&["programmer", "init", "--resume", UUID]).is_err());
        assert!(parse(&["programmer", "--resume"]).is_ok());
        assert!(parse(&["programmer", "--resume", UUID, "run", "hello"]).is_err());
    }

    #[test]
    fn run_requires_exactly_one_prompt_source() {
        assert!(parse(&["programmer", "run"]).is_err());
        assert!(parse(&["programmer", "run", "inline", "--prompt-file", "prompt.txt"]).is_err());
    }

    #[test]
    fn run_accepts_answers_without_a_prompt() {
        const UUID: &str = "12345678-1234-4234-8234-123456789abc";
        for session in [vec!["--continue"], vec!["--resume", UUID]] {
            for answer in ["--answer", "--answer-file"] {
                let mut argv = vec!["programmer", "run"];
                argv.extend(session.clone());
                argv.extend([answer, "response", "--answer-id", "pending-1", "--suspend"]);
                let Some(Command::Run(run)) = parse(&argv).unwrap().command else {
                    unreachable!()
                };
                assert!(run.prompt.is_none());
                assert!(run.suspend);
                assert_eq!(run.answer_id.as_deref(), Some("pending-1"));
                if answer == "--answer" {
                    assert_eq!(run.answer.as_deref(), Some("response"));
                    assert!(run.answer_file.is_none());
                } else {
                    assert_eq!(run.answer_file, Some(PathBuf::from("response")));
                    assert!(run.answer.is_none());
                }
                argv.push("follow-up");
                assert!(parse(&argv).is_ok());
            }
        }
    }

    #[test]
    fn invalid_answer_and_suspension_controls_are_usage_errors() {
        for flags in [
            vec!["--answer", "yes"],
            vec!["--answer-file", "answer.txt"],
            vec![
                "--continue",
                "--answer",
                "yes",
                "--answer-file",
                "answer.txt",
            ],
            vec!["--continue", "--answer-id", "pending-1"],
            vec!["--continue", "--no-save", "--suspend"],
            vec!["--continue", "--no-save", "--work-mode", "manual"],
            vec!["--answer"],
            vec!["--answer-file"],
            vec!["--answer-id"],
        ] {
            let mut argv = vec!["programmer", "run", "hello"];
            argv.extend(flags);
            assert_eq!(parse(&argv).unwrap_err().exit_code(), 2, "{argv:?}");
        }
        assert!(parse(&["programmer", "run", "--suspend"]).is_err());
        assert!(parse(&["programmer", "run", "hello", "--suspend"]).is_ok());
        for flags in [
            vec!["--suspend"],
            vec!["--answer", "yes"],
            vec!["--answer-file", "answer.txt"],
            vec!["--answer-id", "pending-1"],
        ] {
            for command in [vec!["init"], vec!["mcp", "stdio"], vec!["mcp", "http"]] {
                let mut argv = vec!["programmer"];
                argv.extend(command);
                argv.extend(flags.clone());
                assert!(parse(&argv).is_err(), "{argv:?}");
            }
        }
    }

    #[test]
    fn non_interactive_modes_are_validated() {
        assert!(parse(&["programmer", "run", "hello", "--work-mode", "manual"]).is_ok());
        assert!(parse(&["programmer", "init", "--work-mode", "manual"]).is_err());
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

    #[test]
    fn upgrade_parses_check_and_tag() {
        let args = parse(&["programmer", "upgrade"]).unwrap();
        let Some(Command::Upgrade(up)) = args.command else {
            panic!("expected upgrade command");
        };
        assert!(!up.check);
        assert!(up.tag.is_none());

        let args = parse(&["programmer", "upgrade", "--check"]).unwrap();
        let Some(Command::Upgrade(up)) = args.command else {
            panic!("expected upgrade command");
        };
        assert!(up.check);
        assert!(up.tag.is_none());

        let args = parse(&["programmer", "upgrade", "--tag", "v0.3.0"]).unwrap();
        let Some(Command::Upgrade(up)) = args.command else {
            panic!("expected upgrade command");
        };
        assert_eq!(up.tag.as_deref(), Some("v0.3.0"));
    }

    #[test]
    fn uninstall_parses_purge() {
        let args = parse(&["programmer", "uninstall"]).unwrap();
        let Some(Command::Uninstall(un)) = args.command else {
            panic!("expected uninstall command");
        };
        assert!(!un.purge);

        let args = parse(&["programmer", "uninstall", "--purge"]).unwrap();
        let Some(Command::Uninstall(un)) = args.command else {
            panic!("expected uninstall command");
        };
        assert!(un.purge);
    }
    #[test]
    fn security_flags_on_every_headless_command() {
        for command in [
            vec!["run", "hello"],
            vec!["init"],
            vec!["diagnostics"],
            vec!["mcp", "stdio"],
            vec!["mcp", "http"],
        ] {
            let mut argv = vec!["programmer"];
            argv.extend(command);
            let mut removed = argv.clone();
            removed.push("--no-security");
            assert!(parse(&removed).is_err());
            argv.extend([
                "--security-profile",
                "custom",
                "--sandbox",
                "off",
                "--allow-read",
                "read1",
                "--allow-read",
                "read2",
                "--allow-write",
                "write1",
                "--allow-write",
                "write2",
                "--deny-read",
                "deny1",
                "--deny-read",
                "deny2",
                "--deny-env",
                "TOKEN_*",
                "--deny-env",
                "SECRET_*",
                "--no-protect-file-changes",
            ]);
            let security = match parse(&argv).unwrap().command.unwrap() {
                Command::Run(args) => args.agent.security,
                Command::Init(args) => args.agent.security,
                Command::Diagnostics(args) => args.security,
                Command::Mcp(args) => match args.command {
                    McpCommand::Stdio(args) => args.security,
                    McpCommand::Http(args) => args.security,
                },
                _ => unreachable!(),
            };
            assert_eq!(security.security_profile.as_deref(), Some("custom"));
            assert_eq!(security.sandbox.as_deref(), Some("off"));
            assert_eq!(
                security.allow_read,
                [PathBuf::from("read1"), PathBuf::from("read2")]
            );
            assert_eq!(security.allow_write.len(), 2);
            assert_eq!(security.deny_read.len(), 2);
            assert_eq!(security.deny_env, ["TOKEN_*", "SECRET_*"]);
            assert!(security.no_protect_file_changes);
            assert!(security.has_overrides());
        }
        assert!(!SecurityArgs::default().has_overrides());
    }
    #[test]
    fn invocation_overrides_parse_for_run_and_init() {
        for command in [vec!["run", "hello"], vec!["init"]] {
            let mut argv = vec!["programmer"];
            argv.extend(command);
            argv.extend([
                "--config",
                "custom.toml",
                "--vision",
                "--no-memory",
                "--mcp",
                "first",
                "--mcp",
                "second",
                "--allowed-tools",
                "read_file,grep",
                "--allowed-tools",
                "command",
                "--disallowed-tools",
                "command,write_file",
                "--disallowed-tools",
                "fetch",
                "--coauthor",
                "Name <name@example.com>",
                "--soul-file",
                "soul.md",
            ]);
            let agent = match parse(&argv).unwrap().command.unwrap() {
                Command::Run(args) => args.agent,
                Command::Init(args) => args.agent,
                _ => unreachable!(),
            };
            assert_eq!(agent.config, Some(PathBuf::from("custom.toml")));
            assert!(agent.vision && agent.no_memory);
            assert_eq!(agent.mcp, ["first", "second"]);
            assert_eq!(agent.allowed_tools, ["read_file", "grep", "command"]);
            assert_eq!(agent.disallowed_tools, ["command", "write_file", "fetch"]);
            assert_eq!(agent.coauthor.as_deref(), Some("Name <name@example.com>"));
            assert_eq!(agent.soul_file, Some(PathBuf::from("soul.md")));
        }
    }

    #[test]
    fn invocation_override_conflicts_and_missing_values() {
        for command in [vec!["run", "hello"], vec!["init"]] {
            let mut base = vec!["programmer"];
            base.extend(command);
            for conflict in [
                vec!["--vision", "--no-vision"],
                vec!["--mcp", "server", "--no-mcp"],
                vec!["--coauthor", "Name", "--no-coauthor"],
            ] {
                let mut argv = base.clone();
                argv.extend(conflict);
                assert!(parse(&argv).is_err(), "{argv:?}");
            }
            for flag in [
                "--config",
                "--mcp",
                "--allowed-tools",
                "--disallowed-tools",
                "--coauthor",
                "--soul-file",
            ] {
                let mut argv = base.clone();
                argv.push(flag);
                assert!(parse(&argv).is_err(), "{argv:?}");
            }
            base.extend(["--no-vision", "--no-memory", "--no-mcp", "--no-coauthor"]);
            let agent = match parse(&base).unwrap().command.unwrap() {
                Command::Run(args) => args.agent,
                Command::Init(args) => args.agent,
                _ => unreachable!(),
            };
            assert!(agent.no_vision && agent.no_memory && agent.no_mcp && agent.no_coauthor);
        }
    }

    #[test]
    fn run_prompt_additions_are_repeatable() {
        let args = parse(&[
            "programmer",
            "run",
            "hello",
            "--append-prompt-file",
            "one.txt",
            "--append-prompt",
            "first",
            "--append-prompt",
            "second",
            "--append-prompt-file",
            "two.txt",
            "--image",
            "one.png",
            "--image",
            "two.png",
        ])
        .unwrap();
        let Some(Command::Run(run)) = args.command else {
            unreachable!()
        };
        assert_eq!(run.append_prompt, ["first", "second"]);
        assert_eq!(
            run.append_prompt_file,
            [PathBuf::from("one.txt"), PathBuf::from("two.txt")]
        );
        assert_eq!(
            run.image,
            [PathBuf::from("one.png"), PathBuf::from("two.png")]
        );
        assert!(
            parse(&[
                "programmer",
                "run",
                "hello",
                "--image",
                "one.png",
                "--no-vision"
            ])
            .is_err()
        );
        for flag in ["--append-prompt", "--append-prompt-file", "--image"] {
            assert!(parse(&["programmer", "run", "hello", flag]).is_err());
            assert!(parse(&["programmer", "init", flag, "value"]).is_err());
        }
    }

    #[test]
    fn config_is_supported_by_local_tool_commands() {
        for command in [
            vec!["diagnostics"],
            vec!["mcp", "stdio"],
            vec!["mcp", "http"],
        ] {
            let mut argv = vec!["programmer"];
            argv.extend(command);
            argv.push("--config");
            assert!(parse(&argv).is_err());
            argv.push("custom.toml");
            let config = match parse(&argv).unwrap().command.unwrap() {
                Command::Diagnostics(args) => args.config,
                Command::Mcp(args) => match args.command {
                    McpCommand::Stdio(args) => args.config,
                    McpCommand::Http(args) => args.config,
                },
                _ => unreachable!(),
            };
            assert_eq!(config, Some(PathBuf::from("custom.toml")));
        }
    }
}
