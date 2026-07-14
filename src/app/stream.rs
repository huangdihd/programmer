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
use super::helpers;
use crate::response::message_item::MessageItem;
use crate::response::partial_response::PartialResponse;
use crate::ui::components::conversation_panel::conversation_panel::ActivePhase;
use crate::ui::event::{AppEvent, Event};
use async_openai::error::OpenAIError;
use async_openai::types::responses::{CreateResponse, OutputItem, ResponseStreamEvent};
use futures::StreamExt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Spawns a streaming response request for the current conversation state.
pub(crate) fn spawn_stream(app: &mut App<'_>) {
    let cancel_token = Arc::new(AtomicBool::new(false));
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
    let retrying = app.stream_retrying.clone();
    retrying.store(false, Ordering::Relaxed);
    let input_param = app.conversation_panel.get_input_param(
        &app.current_model,
        app.skill_registry.combined_prompt().as_deref(),
    );
    let mcp = app.mcp_manager.clone();
    tokio::spawn(async move {
        let mut request = CreateResponse::default();
        request.stream = Option::from(true);
        request.input = input_param;
        request.model = Option::from(model_name);
        request.tools = Some(crate::tools::tools(mcp.as_deref()));

        const MAX_RETRIES: u32 = 10;
        let mut attempt: u32 = 0;
        let stream = loop {
            match client.responses().create_stream(request.clone()).await {
                Ok(stream) => break Ok(stream),
                Err(e) if helpers::is_retryable(&e) && attempt < MAX_RETRIES => {
                    if cancel_token.load(Ordering::Relaxed) {
                        retrying.store(false, Ordering::Relaxed);
                        return;
                    }
                    attempt += 1;
                    retrying.store(true, Ordering::Relaxed);
                    tokio::time::sleep(helpers::backoff_delay(attempt)).await;
                }
                Err(e) => break Err(e),
            }
        };
        retrying.store(false, Ordering::Relaxed);
        match stream {
            Ok(mut response_stream) => {
                while let Some(response_stream_event) = response_stream.next().await {
                    if cancel_token.load(Ordering::Relaxed) {
                        return;
                    }
                    match response_stream_event {
                        Ok(response_event) => {
                            let _ = sender
                                .send(Event::App(AppEvent::ChunkReceived(response_event)));
                        }
                        Err(openai_error) => {
                            let _ = sender
                                .send(Event::App(AppEvent::OpenAIErrorReceived(openai_error)));
                        }
                    }
                }
            }
            Err(openai_error) => {
                if !cancel_token.load(Ordering::Relaxed) {
                    let _ =
                        sender.send(Event::App(AppEvent::OpenAIErrorReceived(openai_error)));
                }
            }
        }
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
    let base_index = app.conversation_panel.items.len();
    for &live_idx in &app.conversation_panel.live_expanded_items {
        app.conversation_panel
            .expanded_items
            .insert(base_index + live_idx);
    }
    let cancelled = partial_response.cancelled.load(Ordering::Relaxed);
    let usage = partial_response.usage;
    app.conversation_panel.items.extend(
        partial_response
            .items
            .iter()
            .flatten()
            .filter(|item| {
                !cancelled || !matches!(item, OutputItem::FunctionCall(_))
            })
            .map(|item| MessageItem::Output(item.clone().into())),
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

/// Cancel the current stream and clear the receiving state.
#[allow(dead_code)]
pub(crate) fn cancel_stream(app: &mut App<'_>) {
    if let Some(receiving) = &app.conversation_panel.receiving_response {
        receiving.cancelled.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    app.conversation_panel.abort_receiving();
    app.conversation_panel.phase = ActivePhase::None;
}
