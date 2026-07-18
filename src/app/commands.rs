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

//! Message sending and slash-command dispatch.

use super::App;
use super::{diagnostics, session, stream};
use crate::classifier::WorkMode;
use crate::commands::Command;
use crate::ui::components::mcp_panel::McpPanel;
use crate::ui::components::provider_panel::ProviderPanel;
use crate::ui::components::skills_panel::SkillsPanel;
use crate::ui::components::todo_panel::TodoPanel;
use crate::ui::event::AppEvent;
use async_openai::types::responses::{InputContent, InputMessage, InputRole, InputTextContent, OutputStatus};
use async_openai::types::responses::MessageItem as ApiMessageItem;

// ---------------------------------------------------------------------------
// Message sending
// ---------------------------------------------------------------------------

/// Collect input, push to history, and start a user request.
pub(crate) async fn send_message(app: &mut App<'_>) {
    let typed = app.input_panel.expanded_content();
    if typed.is_empty() {
        return;
    }
    // History keeps the compact `@path` form; the model receives the referenced
    // file contents appended.
    app.input_panel.push_history(typed.clone());
    app.input_panel.clear();
    let text = crate::commands::expand_file_references(&typed).await;
    start_request(app, text).await;
}

/// Start a turn from a message (with the user role).
pub(crate) async fn start_request(app: &mut App<'_>, text: String) {
    start_request_as(app, text, InputRole::User).await;
}

/// Start a turn from a message with the given role. `User` is a normal user
/// message; `Developer` carries a hidden instruction (like `/init`).
pub(crate) async fn start_request_as(app: &mut App<'_>, text: String, role: InputRole) {
    if app.conversation_panel.is_busy() {
        let is_at_bottom = app.conversation_panel.is_at_bottom();
        match app.conversation_panel.pending_message.as_mut() {
            Some(pending_message) => {
                pending_message.push('\n');
                pending_message.push_str(&text);
            }
            None => app.conversation_panel.pending_message = Some(text),
        }
        if is_at_bottom {
            app.conversation_panel.scroll_to_bottom()
        }
        return;
    }

    let input_message = InputMessage {
        content: vec![InputContent::InputText(InputTextContent {
            text: text.clone(),
        })],
        role,
        status: Option::from(OutputStatus::Completed),
    };

    app.conversation_panel
        .add_input_message(ApiMessageItem::Input(input_message));
    app.conversation_panel.reset_accumulated_usage();
    diagnostics::maybe_seed_diagnostics_baseline(app);
    session::save_session(app);
    // Fresh turn: start from an un-cancelled root token so a prior turn's Esc
    // doesn't carry over to this one.
    app.cancel.active = crate::cancel::CancellationToken::new();
    stream::spawn_stream(app);
}

/// Run a `!command` from the input: spawn it as an interactive PTY task and
/// open the terminal panel focused on it, so the user drives it right away.
pub(crate) fn run_bang_command(app: &mut App<'_>, input: &str) {
    use crate::ui::components::terminal_panel::TerminalPane;

    let command = input.strip_prefix('!').unwrap_or(input).trim().to_string();
    if command.is_empty() {
        app.input_panel.clear();
        return;
    }
    app.input_panel.push_history(input.to_string());
    app.input_panel.clear();

    // Spawn at the size the terminal panel will render at, so the first frame
    // doesn't have to resize the fresh PTY. A resize racing the child's
    // startup leaves a SIGWINCH pending from the fork/exec window, which the
    // kernel then delivers at the worst moment (e.g. inside Python 3.14's
    // REPL `tcsetattr`, which dies on EINTR).
    let (rows, cols) = crossterm::terminal::size()
        .map(|(w, h)| (h.saturating_sub(2).max(1), w.max(1)))
        .unwrap_or((24, 80));
    match crate::tasks::spawn_interactive(&command, None, Some(&command), rows, cols) {
        Ok(id) => {
            let mut pane = TerminalPane::new(id, command);
            // Grab input immediately — the user typed `!` to interact.
            pane.grabbed = true;
            app.terminal_pane = Some(pane);
        }
        Err(e) => app.conversation_panel.add_error_string(e),
    }
}

/// `/compact`: ask the model for a continuation summary of the conversation so
/// far, then (in [`super::events`]'s `CompactFinished` handler) install it as a
/// context boundary — the model afterwards sees the summary instead of the
/// summarized history, while the UI keeps everything visible.
pub(crate) fn start_compact(app: &mut App<'_>) {
    use crate::ui::components::conversation_panel::conversation_panel::ActivePhase;
    use crate::ui::event::Event;
    use async_openai::types::responses::{CreateResponse, InputItem, InputParam, Item, OutputItem, OutputMessageContent};

    if app.conversation_panel.is_busy() {
        app.conversation_panel
            .add_warning_string("cannot compact while a turn is in flight");
        return;
    }
    if !app.conversation_panel.has_compactable_history() {
        app.conversation_panel
            .add_info_string("nothing to compact yet".to_string());
        return;
    }
    let (client, model_name) = match app.provider_manager.resolve(&app.current_model) {
        Some((c, m)) => (c.clone(), m),
        None => {
            app.conversation_panel
                .add_error_string(format!("unknown provider/model: {}", app.current_model));
            return;
        }
    };

    // The full current context plus the summarization instruction. No tools:
    // the model must answer with the summary text, not act.
    let mut input_items = match app.conversation_panel.get_input_param(
        &app.current_model,
        None,
        None,
        None,
    ) {
        InputParam::Items(items) => items,
        InputParam::Text(text) => vec![InputItem::from(Item::Message(
            ApiMessageItem::Input(InputMessage {
                content: vec![InputContent::InputText(InputTextContent { text })],
                role: InputRole::User,
                status: Some(OutputStatus::Completed),
            }),
        ))],
    };
    input_items.push(InputItem::from(Item::Message(ApiMessageItem::Input(
        InputMessage {
            content: vec![InputContent::InputText(InputTextContent {
                text: crate::prompts::COMPACT_PROMPT.to_string(),
            })],
            role: InputRole::User,
            status: Some(OutputStatus::Completed),
        },
    ))));

    app.conversation_panel.phase = ActivePhase::Compacting;
    app.cancel.active = crate::cancel::CancellationToken::new();
    let cancel_token = app.cancel.active.child();
    let sender = app.events.sender.clone();
    tokio::spawn(async move {
        let request = CreateResponse {
            input: InputParam::Items(input_items),
            model: Some(model_name),
            ..Default::default()
        };
        let result = match client.responses().create(request).await {
            Ok(response) => {
                let text = response
                    .output
                    .iter()
                    .filter_map(|item| match item {
                        OutputItem::Message(msg) => Some(msg.content.iter().filter_map(
                            |c| match c {
                                OutputMessageContent::OutputText(t) => Some(t.text.as_str()),
                                _ => None,
                            },
                        )),
                        _ => None,
                    })
                    .flatten()
                    .collect::<Vec<_>>()
                    .join("\n");
                if text.trim().is_empty() {
                    Err("the model returned an empty summary".to_string())
                } else {
                    Ok(text)
                }
            }
            Err(e) => Err(e.to_string()),
        };
        if !cancel_token.is_cancelled() {
            let _ = sender.send(Event::App(crate::ui::event::AppEvent::CompactFinished(
                result,
                cancel_token,
            )));
        }
    });
}

/// Open the interactive terminal panel for a task. With no argument, opens the
/// sole interactive task; with an id, opens that task if it is interactive.
fn open_terminal(app: &mut App<'_>, arg: &str) {
    use crate::ui::components::terminal_panel::TerminalPane;

    // Accept an id as the first token (completion may append the task name).
    let first = arg.split_whitespace().next().unwrap_or("");
    let id = if first.is_empty() {
        // Auto-select the sole *running* interactive task.
        let running: Vec<u64> = crate::tasks::snapshot_all()
            .iter()
            .filter(|t| {
                t.status == crate::tasks::TaskStatus::Running
                    && crate::tasks::is_interactive(t.id)
            })
            .map(|t| t.id)
            .collect();
        match running.as_slice() {
            [only] => *only,
            [] => {
                app.conversation_panel.add_warning_string(
                    "no running interactive task — create one with the task tool (interactive: true)",
                );
                return;
            }
            _ => {
                app.conversation_panel.add_warning_string(
                    "multiple running interactive tasks — specify one with /terminal <id>",
                );
                return;
            }
        }
    } else {
        match first.parse::<u64>() {
            Ok(id) => id,
            Err(_) => {
                app.conversation_panel
                    .add_warning_string(format!("/terminal: '{first}' is not a task id"));
                return;
            }
        }
    };

    if !crate::tasks::is_interactive(id) {
        app.conversation_panel.add_warning_string(format!(
            "task {id} is not interactive — only PTY tasks can be opened in the terminal"
        ));
        return;
    }
    let name = crate::tasks::snapshot(id)
        .map(|s| s.name)
        .unwrap_or_default();
    app.terminal_pane = Some(TerminalPane::new(id, name));
}

// ---------------------------------------------------------------------------
// Slash-command dispatch
// ---------------------------------------------------------------------------

/// Parse and execute a slash command. If the command is unknown, fall back
/// to sending it to the AI model.
pub(crate) async fn execute_command(app: &mut App<'_>, input: &str) {
    let command = Command::parse(input);
    app.input_panel.completion = None;
    let is_known = command.is_some();

    match command {
        Some(Command::Quit) => {
            app.input_panel.clear();
            app.quit();
        }
        Some(Command::Clear) => {
            app.input_panel.clear();
            app.conversation_panel.clear_messages();
            diagnostics::reset_diagnostics_state(app);
            session::delete_session(app);
            app.todo_list = crate::todos::TodoList::default();
            crate::todos::TodoList::clear_file();
            session::save_session(app);
        }
        Some(Command::New) => {
            app.input_panel.clear();
            session::save_session(app);
            app.conversation_panel.clear_messages();
            diagnostics::reset_diagnostics_state(app);
            if let Some(mgr) = &app.session.mgr {
                let new_session = mgr.create();
                app.session.uuid = new_session.uuid;
            }
            app.todo_list = crate::todos::TodoList::default();
            crate::todos::TodoList::clear_file();
            app.conversation_panel
                .add_info_string("Started a new session. Previous session saved.".to_string());
            session::save_session(app);
        }
        Some(Command::Model(model)) => {
            app.input_panel.clear();
            let model = model.trim().to_string();
            if model.is_empty() {
                app.conversation_panel.add_info_string(
                    "usage: /model <provider/model> — e.g. /model openai/gpt-4o",
                );
                session::save_session(app);
                return;
            }
            match app.provider_manager.resolve(&model) {
                Some(_) => {
                    app.current_model = model;
                    app.conversation_panel
                        .add_info_string(format!("switched to model: {}", app.current_model));
                }
                None => {
                    app.conversation_panel.add_error_string(format!(
                        "unknown provider/model: {model} — use /providers to list available",
                    ));
                }
            }
            session::save_session(app);
        }
        Some(Command::Mode(arg)) => {
            app.input_panel.clear();
            let prev = app.work_mode;
            match arg.trim().to_lowercase().as_str() {
                "manual" => app.work_mode = WorkMode::Manual,
                "auto" => app.work_mode = WorkMode::Auto,
                "plan" => app.work_mode = WorkMode::Plan,
                "yolo" => {
                    if app.config.allow_yolo {
                        app.work_mode = WorkMode::Yolo;
                    } else {
                        app.conversation_panel.add_error_string(
                            "YOLO mode runs every tool call unchecked and is \
                             disabled by default — set `allow_yolo = true` in \
                             config to enable it"
                                .to_string(),
                        );
                        return;
                    }
                }
                "" => app.work_mode = app.work_mode.next(app.config.allow_yolo),
                other => {
                    app.conversation_panel.add_error_string(format!(
                        "unknown mode '{other}' — use manual, auto, plan, or yolo"
                    ));
                    return;
                }
            }
            if app.work_mode != prev {
                session::persist_config(app);
            }
        }
        Some(Command::Classifier(arg)) => {
            app.input_panel.clear();
            let arg = arg.trim().to_string();
            match arg.as_str() {
                "" => {
                    let current = app
                        .config
                        .classifier_model
                        .clone()
                        .unwrap_or_else(|| format!("{} (chat model)", app.current_model));
                    app.conversation_panel.add_info_string(format!(
                        "classifier model: {current}\n\
                         usage: /classifier <provider/model> to set, \
                         /classifier clear to reset to the chat model"
                    ));
                }
                "clear" | "default" | "reset" => {
                    app.config.classifier_model = None;
                    app.conversation_panel.add_info_string(
                        "classifier model reset — Auto mode now uses the chat model",
                    );
                    session::persist_config(app);
                }
                model => match app.provider_manager.resolve(model) {
                    Some(_) => {
                        app.config.classifier_model = Some(model.to_string());
                        app.conversation_panel
                            .add_info_string(format!("classifier model set to: {model}"));
                        session::persist_config(app);
                    }
                    None => {
                        app.conversation_panel.add_error_string(format!(
                            "unknown provider/model: {model} — use /providers to list available"
                        ));
                    }
                },
            }
            session::save_session(app);
        }
        Some(Command::Init) => {
            app.input_panel.clear();
            // /init doesn't stack — if a turn is already running, queue like any
            // other message.
            if app.conversation_panel.is_busy() {
                app.conversation_panel.pending_message = Some(super::helpers::init_prompt());
                return;
            }
            app.conversation_panel
                .add_info_string("Scanning project and setting up diagnostics…");
            let _tokio_handle = tokio::spawn({
                let sender = app.events.sender.clone();
                async move {
                    // Let the info_string render before we push the init prompt.
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    let _ = sender.send(crate::ui::event::Event::App(AppEvent::StartInit));
                }
            });
        }
        Some(Command::Session) => {
            app.input_panel.clear();
            session::save_session(app);
        }
        Some(Command::Todo) => {
            app.input_panel.clear();
            app.todo_panel = Some(TodoPanel::new(app.todo_list.clone()));
        }
        Some(Command::Terminal(arg)) => {
            app.input_panel.clear();
            open_terminal(app, &arg);
        }
        Some(Command::Compact) => {
            app.input_panel.clear();
            start_compact(app);
        }
        Some(Command::Providers(arg)) => {
            app.input_panel.clear();
            let arg = arg.trim();
            match arg {
                "show" | "list" => {
                    let names: Vec<String> = app
                        .provider_manager
                        .provider_names()
                        .iter()
                        .map(|n| n.to_string())
                        .collect();
                    if names.is_empty() {
                        app.conversation_panel
                            .add_info_string("no providers configured");
                    } else {
                        let mut lines = vec!["Configured providers:".to_string()];
                        for name in &names {
                            let models: Vec<String> = app
                                .provider_manager
                                .models_for(name)
                                .iter()
                                .map(|m| m.to_string())
                                .collect();
                            let models_str = models
                                .iter()
                                .map(|m| format!("    {name}/{m}"))
                                .collect::<Vec<_>>()
                                .join("\n");
                            lines.push(format!("  {name}:"));
                            lines.push(models_str);
                        }
                        app.conversation_panel
                            .add_info_string(lines.join("\n"));
                    }
                }
                "manage" => {
                    app.provider_panel = Some(ProviderPanel::new());
                }
                _ => {
                    app.conversation_panel.add_info_string(
                        "usage: /providers show — list configured providers\n\
                         \u{20}      /providers manage — open the management panel",
                    );
                }
            }
            session::save_session(app);
        }
        Some(Command::Skill(arg)) => {
            app.input_panel.clear();
            let arg = arg.trim();
            match arg {
                "list" | "show" => {
                    let names = app.skill_registry.names();
                    if names.is_empty() {
                        app.conversation_panel
                            .add_info_string("no skills configured");
                    } else {
                        let mut lines: Vec<String> = names
                            .iter()
                            .map(|name| {
                                let active = if app.skill_registry.is_active(name) {
                                    " [active]"
                                } else {
                                    ""
                                };
                                format!("  {name}{active}")
                            })
                            .collect();
                        lines.insert(0, "Skills:".to_string());
                        app.conversation_panel
                            .add_info_string(lines.join("\n"));
                    }
                }
                "off" | "clear" | "none" => {
                    app.skill_registry.clear();
                    app.conversation_panel
                        .add_info_string("skills deactivated");
                    session::save_session(app);
                }
                "manage" => {
                    app.skills_panel = Some(SkillsPanel::new());
                }
                "" => {
                    let active = app.skill_registry.activated_names().join(", ");
                    if active.is_empty() {
                        app.conversation_panel.add_info_string(
                            "no skills active — use /skill <name> to activate, \
                             /skill list to see available",
                        );
                    } else {
                        app.conversation_panel
                            .add_info_string(format!("active skills: {active}"));
                    }
                }
                name => {
                    if app.skill_registry.activate(name) {
                        app.conversation_panel
                            .add_info_string(format!("skill activated: {name}"));
                        session::save_session(app);
                    } else {
                        app.conversation_panel
                            .add_error_string(format!("unknown skill: {name}"));
                    }
                }
            }
            session::save_session(app);
        }
        Some(Command::Mcp(arg)) => {
            app.input_panel.clear();
            let arg = arg.trim();
            match arg {
                "show" | "list" => {
                    if app.config.mcp_servers.is_empty() {
                        app.conversation_panel
                            .add_info_string("no MCP servers configured");
                    } else {
                        let mut lines: Vec<String> = Vec::new();
                        for srv in &app.config.mcp_servers {
                            let policy = match srv.auto_approve {
                                crate::mcp::types::McpPolicy::Trusted => "trusted",
                                crate::mcp::types::McpPolicy::Review => "review",
                            };
                            lines.push(format!(
                                "  {} ({}:{policy})",
                                srv.name, srv.command
                            ));
                        }
                        lines.insert(0, "MCP servers:".to_string());
                        app.conversation_panel
                            .add_info_string(lines.join("\n"));
                    }
                }
                "manage" => {
                    app.mcp_panel = Some(McpPanel::new());
                }
                _ => {
                    app.conversation_panel.add_info_string(
                        "usage: /mcp show — list MCP servers and their status\n\
                         \u{20}      /mcp manage — open the management panel",
                    );
                }
            }
            session::save_session(app);
        }
        Some(Command::Help) => {
            app.input_panel.clear();
            let mut lines: Vec<String> = Command::descriptions()
                .iter()
                .map(|(cmd, desc)| format!("  {cmd:35} {desc}"))
                .collect();
            lines.insert(0, "Available commands:".to_string());
            app.conversation_panel.add_info_string(lines.join("\n"));
            session::save_session(app);
        }
        Some(Command::Plan(arg)) => {
            app.input_panel.clear();
            match arg.trim().to_lowercase().as_str() {
                "approve" | "ok" | "go" => {
                    if app.work_mode == WorkMode::Plan
                        && app.plan_phase == crate::classifier::PlanPhase::Reviewing
                    {
                        // Exit Plan mode into Auto.
                        app.work_mode = WorkMode::Auto;
                        app.plan_phase = crate::classifier::PlanPhase::default();
                        app.conversation_panel
                            .add_info_string("Plan approved — executing with Auto mode.");
                        let hidden = "The plan was approved by the user. Execute it now using the identified steps.";
                        session::save_session(app);
                        start_request_as(app, hidden.to_string(), InputRole::Developer).await;
                    } else {
                        app.conversation_panel
                            .add_info_string("No plan pending approval. Use /mode plan to enter Plan mode.");
                    }
                }
                "cancel" | "abort" => {
                    app.work_mode = WorkMode::Auto;
                    app.plan_phase = crate::classifier::PlanPhase::default();
                    app.conversation_panel
                        .add_info_string("Plan cancelled — returned to Auto mode.");
                    session::persist_config(app);
                }
                _ => {
                    app.conversation_panel.add_info_string(
                        "usage: /plan approve — approve current plan and execute\n\
                         \u{20}      /plan cancel — cancel plan and return to Auto",
                    );
                }
            }
            session::save_session(app);
        }
        None => {
            // Unknown slash-command; send it to the AI as a normal message.
            app.events.send(AppEvent::Start);
        }
    }

    if is_known {
        app.input_panel.push_history(input.to_string());
    }
}
