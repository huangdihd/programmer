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

//! Streaming API request lifecycle: spawn, handle chunks, handle errors.

use super::App;
use crate::classifier::{PlanPhase, WorkMode};
use crate::response::message_item::MessageItem;
use crate::response::partial_response::PartialResponse;
use crate::ui::components::conversation_panel::conversation_panel::ActivePhase;
use crate::ui::event::{AppEvent, Event};
use async_openai::error::OpenAIError;
use async_openai::types::responses::{OutputItem, ResponseStreamEvent};

/// Build an optional plan-mode system prompt snippet.
fn plan_system_prompt(app: &App<'_>) -> Option<&'static str> {
    if app.work_mode != WorkMode::Plan {
        return None;
    }
    match app.plan_phase {
        PlanPhase::Planning => Some(PLAN_PLANNING_PROMPT),
        PlanPhase::Reviewing => None,
    }
}

use crate::prompts::PLAN_PLANNING_PROMPT;

/// Spawns a streaming response request for the current conversation state.
pub(crate) fn spawn_stream(app: &mut App<'_>) {
    // Derive this stream segment's token from the turn's root token so Esc
    // (which cancels the root) stops it, even across tool-loop continuations.
    let cancel_token = app.cancel.active.child();
    app.conversation_panel.live_expanded_items.clear();
    app.conversation_panel.phase = ActivePhase::None;
    app.conversation_panel.receiving_response =
        Some(PartialResponse::new(cancel_token.clone()));
    let (client, model_name) = match app.provider_manager.resolve(&app.current_model) {
        Some((c, m)) => (c.clone(), m),
        None => {
            app.conversation_panel.abort_receiving();
            app.conversation_panel
                .add_error_string(format!("unknown provider/model: {}", app.current_model));
            return;
        }
    };
    let sender = app.events.sender.clone();
    let retrying = app.cancel.stream_retrying.clone();

    // Build the request from the shared primitive so it matches what the
    // headless engine would send for the same history.
    let skill_prompt = app.skill_registry.combined_prompt();
    let ctx = crate::engine::request::SystemContext {
        current_model: &app.current_model,
        skill_prompt: skill_prompt.as_deref(),
        plan_prompt: plan_system_prompt(app),
        coauthor: app.config.git_coauthor.as_deref(),
    };
    let tools = crate::tools::tools(app.mcp_manager.as_deref());
    let request = crate::engine::request::build_request(
        &app.conversation_panel.conversation,
        &ctx,
        model_name,
        tools,
    );

    tokio::spawn(async move {
        crate::engine::stream::stream_with_retries(
            &client,
            &request,
            &cancel_token,
            &retrying,
            |result| {
                let event = match result {
                    Ok(response_event) => AppEvent::ChunkReceived(response_event),
                    Err(openai_error) => AppEvent::OpenAIErrorReceived(openai_error),
                };
                let _ = sender.send(Event::App(event));
            },
        )
        .await;
    });
}

/// Handle a successful streaming chunk event.
pub(crate) async fn handle_chunk_events(
    app: &mut App<'_>,
    response_stream_event: ResponseStreamEvent,
) {
    if app.conversation_panel.receiving_response.is_none() {
        return;
    }
    let Some(partial_response) = app
        .conversation_panel
        .handle_response_stream_event(response_stream_event)
    else {
        if let Some(ref receiving) = app.conversation_panel.receiving_response {
            if receiving.has_function_calls() {
                app.conversation_panel.phase = ActivePhase::CreatingToolCall;
            } else if receiving.has_message_items() {
                app.conversation_panel.phase = ActivePhase::Outputting;
            }
        }
        return;
    };
    let base_index = app.conversation_panel.conversation.items.len();
    for &live_idx in &app.conversation_panel.live_expanded_items {
        app.conversation_panel
            .expanded_items
            .insert(base_index + live_idx);
    }
    let cancelled = partial_response.cancelled.is_cancelled();
    let usage = partial_response.usage;
    app.conversation_panel.conversation.items.extend(
        partial_response
            .items
            .iter()
            .flatten()
            .filter(|item| {
                !cancelled || !matches!(item, OutputItem::FunctionCall(_))
            })
            .map(|item| MessageItem::Output(item.clone())),
    );
    if let Some((input, output)) = usage {
        app.conversation_panel.add_usage(input, output);
    }
    app.events
        .send(AppEvent::ResponseFinished(partial_response));
}

/// Handle an error from the streaming connection.
pub(crate) async fn handle_error_events(app: &mut App<'_>, error: OpenAIError) {
    if !app.conversation_panel.is_busy() {
        return;
    }
    app.conversation_panel.abort_receiving();
    app.conversation_panel.phase = ActivePhase::None;
    app.conversation_panel.flush_usage();
    app.conversation_panel.add_error(error);
    if let Some(pending_request) = app.conversation_panel.pending_message.take() {
        super::commands::start_request(app, pending_request).await;
    }
}

