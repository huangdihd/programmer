// Copyright (C) 2026 huangdihd
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

//! Synchronous compaction at a suspended headless runner boundary.
use std::sync::{Arc, Mutex};

use crate::{cancel::CancellationToken, conversation::Conversation, runner::RunnerError};
use async_openai::types::responses::InputParam;

#[derive(Default)]
struct State {
    responses: usize,
    last_attempt: Option<usize>,
}

pub(super) struct Compactor {
    pub auto_tokens: u32,
    pub mandatory_tokens: u32,
    pub keep_recent_turns: usize,
    pub model: String,
    pub soul: Option<String>,
    pub vision: bool,
    pub thinking: crate::thinking::ThinkingLevel,
    pub target: Option<(
        async_openai::Client<async_openai::config::OpenAIConfig>,
        String,
    )>,
    state: Mutex<State>,
}

impl Compactor {
    pub fn new(
        config: &crate::config::programmer_config::ProgrammerConfig,
        model: &str,
        providers: &crate::providers::ProviderManager,
        thinking: crate::thinking::ThinkingLevel,
    ) -> Self {
        let model = config.compact_model.as_deref().unwrap_or(model).to_owned();
        let target = providers
            .resolve(&model)
            .map(|(client, name)| (client.clone(), name));
        Self {
            auto_tokens: config.auto_compact_tokens,
            mandatory_tokens: config.mandatory_compact_tokens,
            keep_recent_turns: config.compact_keep_recent_turns,
            soul: config.soul.clone(),
            vision: config.vision_enabled,
            thinking,
            model,
            target,
            state: Mutex::default(),
        }
    }

    pub async fn run(
        &self,
        input_tokens: u32,
        conversation: &Arc<Mutex<Conversation>>,
        cancel: &CancellationToken,
        jsonl: bool,
    ) -> Result<bool, RunnerError> {
        self.run_with(
            input_tokens,
            conversation,
            cancel,
            |input| async move {
                let (client, name) = self
                    .target
                    .as_ref()
                    .ok_or_else(|| format!("unknown provider/model: {}", self.model))?;
                let request = crate::compaction::build_compact_request(
                    crate::compaction::compact_input_items(input),
                    name.clone(),
                    self.thinking,
                );
                crate::compaction::stream_compact_response(client, request, cancel.clone())
                    .await
                    .map(|result| result.summary)
            },
            |phase, error| {
                if jsonl {
                    println!(
                        "{}",
                        serde_json::json!({
                            "schema_version": 1, "type": "compaction", "phase": phase,
                            "input_tokens": input_tokens, "model": self.model, "error": error,
                        })
                    );
                }
            },
        )
        .await
    }

    async fn run_with<F, Fut, E>(
        &self,
        input_tokens: u32,
        conversation: &Arc<Mutex<Conversation>>,
        cancel: &CancellationToken,
        generate: F,
        event: E,
    ) -> Result<bool, RunnerError>
    where
        F: FnOnce(InputParam) -> Fut,
        Fut: std::future::Future<Output = Result<String, String>>,
        E: Fn(&str, Option<&str>),
    {
        let mandatory = self.mandatory_tokens != 0 && input_tokens >= self.mandatory_tokens;
        {
            let mut state = self.state.lock().unwrap();
            state.responses += 1;
            if !mandatory
                && (self.auto_tokens == 0
                    || input_tokens < self.auto_tokens
                    || state.last_attempt.is_some_and(|last| {
                        state.responses - last < crate::consts::HEADLESS_COMPACT_COOLDOWN_RESPONSES
                    }))
            {
                return Ok(false);
            }
        }
        let snapshot = {
            let conv = conversation.lock().unwrap();
            // Mandatory compaction includes completed work in the active turn,
            // exactly like the TUI's suspended mandatory boundary.
            conv.compaction_cutoff(if mandatory { 0 } else { self.keep_recent_turns })
                .map(|cutoff| {
                    (
                        cutoff,
                        conv.input_param_for_prefix(
                            cutoff,
                            &self.model,
                            self.soul.as_deref(),
                            self.vision,
                        ),
                    )
                })
        };
        let Some((cutoff, input)) = snapshot else {
            return Ok(false);
        };
        {
            let mut state = self.state.lock().unwrap();
            state.last_attempt = Some(state.responses);
        }
        event("started", None);
        let result = cancel.wait_or(generate(input)).await;
        if cancel.is_cancelled() || result.is_none() {
            event("failed", Some("cancelled"));
            return Err(RunnerError::Cancelled);
        }
        match result.expect("checked above").and_then(|summary| {
            if summary.trim().is_empty() {
                Err("the model returned an empty summary".into())
            } else if conversation
                .lock()
                .unwrap()
                .apply_compaction_at(cutoff, summary)
            {
                Ok(())
            } else {
                Err("conversation changed during compaction".into())
            }
        }) {
            Ok(()) => {
                event("finished", None);
                Ok(true)
            }
            Err(error) => {
                event("failed", Some(&error));
                if mandatory {
                    Err(RunnerError::Compaction(error))
                } else {
                    eprintln!("notice: automatic context compaction failed: {error}");
                    Ok(false)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::response::message_item::MessageItem;
    use async_openai::types::responses::{
        AssistantRole, InputContent, InputMessage, InputRole, MessageItem as ApiMessageItem,
        OutputItem, OutputMessage, OutputMessageContent, OutputStatus, OutputTextContent,
    };

    fn compactor(auto_tokens: u32, mandatory_tokens: u32, keep_recent_turns: usize) -> Compactor {
        Compactor {
            auto_tokens,
            mandatory_tokens,
            keep_recent_turns,
            model: "test/model".into(),
            soul: None,
            vision: false,
            thinking: Default::default(),
            target: None,
            state: Mutex::default(),
        }
    }

    fn add_turn(conv: &mut Conversation, text: &str) {
        conv.add_input_message(ApiMessageItem::Input(InputMessage {
            content: vec![InputContent::InputText(text.into())],
            role: InputRole::User,
            status: Some(OutputStatus::Completed),
        }));
        conv.add_output(OutputItem::Message(OutputMessage {
            content: vec![OutputMessageContent::OutputText(OutputTextContent {
                annotations: vec![],
                logprobs: None,
                text: format!("answer: {text}"),
            })],
            id: "msg_test".into(),
            role: AssistantRole::Assistant,
            phase: None,
            status: OutputStatus::Completed,
        }));
    }

    fn conversation() -> Arc<Mutex<Conversation>> {
        let mut conv = Conversation::new();
        add_turn(&mut conv, "old work");
        add_turn(&mut conv, "recent work");
        Arc::new(Mutex::new(conv))
    }

    fn history(conv: &Arc<Mutex<Conversation>>) -> String {
        format!("{:?}", conv.lock().unwrap().items().collect::<Vec<_>>())
    }

    #[tokio::test]
    async fn thresholds_are_inclusive_and_zero_disables_each_threshold() {
        for (auto, mandatory, tokens, expected) in [
            (100, 0, 99, false),
            (100, 0, 100, true),
            (0, 100, 99, false),
            (0, 100, 100, true),
            (0, 0, u32::MAX, false),
            (0, 0, 0, false),
        ] {
            let calls = std::cell::Cell::new(0);
            let result = compactor(auto, mandatory, 0)
                .run_with(
                    tokens,
                    &conversation(),
                    &CancellationToken::new(),
                    |_| {
                        calls.set(calls.get() + 1);
                        async { Ok("summary".into()) }
                    },
                    |_, _| {},
                )
                .await
                .unwrap();
            assert_eq!(
                result, expected,
                "auto={auto}, mandatory={mandatory}, tokens={tokens}"
            );
            assert_eq!(calls.get(), usize::from(expected));
        }
    }

    #[tokio::test]
    async fn automatic_attempts_wait_four_responses_even_after_failure() {
        let compactor = compactor(100, 0, 0);
        let conv = conversation();
        let cancel = CancellationToken::new();
        let calls = std::cell::Cell::new(0);
        for response in 1..=5 {
            assert!(
                !compactor
                    .run_with(
                        100,
                        &conv,
                        &cancel,
                        |_| {
                            calls.set(calls.get() + 1);
                            async { Err("retry later".into()) }
                        },
                        |_, _| {}
                    )
                    .await
                    .unwrap()
            );
            assert_eq!(calls.get(), if response < 5 { 1 } else { 2 });
        }
    }

    #[tokio::test]
    async fn mandatory_bypasses_cooldown_and_snapshots_active_work() {
        let compactor = compactor(100, 200, 1);
        let conv = conversation();
        let cancel = CancellationToken::new();
        assert!(
            compactor
                .run_with(
                    100,
                    &conv,
                    &cancel,
                    |_| async { Ok("old summary".into()) },
                    |_, _| {}
                )
                .await
                .unwrap()
        );
        add_turn(&mut conv.lock().unwrap(), "active completed work");
        let before = history(&conv);
        assert!(
            compactor
                .run_with(
                    200,
                    &conv,
                    &cancel,
                    |input| {
                        let snapshot = format!("{input:?}");
                        assert!(snapshot.contains("old summary"));
                        assert!(snapshot.contains("recent work"));
                        assert!(snapshot.contains("active completed work"));
                        assert_eq!(history(&conv), before, "snapshot must not mutate history");
                        async { Ok("mandatory summary".into()) }
                    },
                    |_, _| {}
                )
                .await
                .unwrap()
        );
        let guard = conv.lock().unwrap();
        let input = format!("{:?}", guard.to_input_param("test/model", None, None, None));
        assert!(input.contains("mandatory summary"));
        assert!(!input.contains("active completed work"));
        assert!(history_without_lock(&guard).contains("active completed work"));
    }

    fn history_without_lock(conv: &Conversation) -> String {
        format!("{:?}", conv.items().collect::<Vec<_>>())
    }

    #[tokio::test]
    async fn auto_keeps_recent_turns_and_serializes_the_shared_compacted_marker() {
        let compactor = compactor(100, 0, 1);
        let conv = conversation();
        let original: Vec<_> = conv
            .lock()
            .unwrap()
            .items()
            .map(|item| format!("{item:?}"))
            .collect();
        assert!(
            compactor
                .run_with(
                    100,
                    &conv,
                    &CancellationToken::new(),
                    |input| {
                        let snapshot = format!("{input:?}");
                        assert!(snapshot.contains("old work"));
                        assert!(!snapshot.contains("recent work"));
                        async { Ok("retained summary".into()) }
                    },
                    |_, _| {}
                )
                .await
                .unwrap()
        );
        let guard = conv.lock().unwrap();
        let retained: Vec<_> = guard
            .items()
            .filter(|item| !matches!(item, MessageItem::Compacted { .. }))
            .map(|item| format!("{item:?}"))
            .collect();
        assert_eq!(retained, original);
        let marker = guard
            .items()
            .find(|item| matches!(item, MessageItem::Compacted { .. }))
            .unwrap();
        let serialized = crate::session::SerializableMessageItem::from(marker.clone());
        let json = serde_json::to_value(serialized).unwrap();
        assert_eq!(json["variant"], "Compacted");
        assert_eq!(json["payload"]["summary"], "retained summary");
        let input = format!("{:?}", guard.to_input_param("test/model", None, None, None));
        assert!(input.contains("retained summary"));
        assert!(input.contains("recent work"));
        assert!(!input.contains("old work"));
    }

    #[tokio::test]
    async fn failure_continues_auto_but_stops_mandatory() {
        for mandatory in [false, true] {
            for failure in [Err("provider failed".into()), Ok("  \n".into())] {
                let compactor = compactor(100, if mandatory { 100 } else { 0 }, 0);
                let conv = conversation();
                let before = history(&conv);
                let events = Mutex::new(Vec::new());
                let result = compactor
                    .run_with(
                        100,
                        &conv,
                        &CancellationToken::new(),
                        |_| async { failure },
                        |phase, error| {
                            events
                                .lock()
                                .unwrap()
                                .push((phase.to_owned(), error.map(str::to_owned)))
                        },
                    )
                    .await;
                if mandatory {
                    assert!(matches!(result, Err(RunnerError::Compaction(_))));
                } else {
                    assert!(matches!(result, Ok(false)));
                }
                assert_eq!(history(&conv), before);
                let events = events.lock().unwrap();
                assert_eq!(events.len(), 2);
                assert_eq!(events[0], ("started".into(), None));
                assert_eq!(events[1].0, "failed");
                assert!(events[1].1.is_some());
            }
        }
    }

    #[tokio::test]
    async fn cancellation_while_summary_is_pending_does_not_apply() {
        let compactor = compactor(100, 0, 0);
        let conv = conversation();
        let before = history(&conv);
        let cancel = CancellationToken::new();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let events = Mutex::new(Vec::new());
        let run = compactor.run_with(
            100,
            &conv,
            &cancel,
            |_| async {
                started_tx.send(()).unwrap();
                std::future::pending::<Result<String, String>>().await
            },
            |phase, error| {
                events
                    .lock()
                    .unwrap()
                    .push((phase.to_owned(), error.map(str::to_owned)))
            },
        );
        let (result, ()) = tokio::join!(run, async {
            started_rx.await.unwrap();
            cancel.cancel();
        });
        assert!(matches!(result, Err(RunnerError::Cancelled)));
        assert_eq!(history(&conv), before);
        assert_eq!(
            *events.lock().unwrap(),
            vec![
                ("started".into(), None),
                ("failed".into(), Some("cancelled".into())),
            ]
        );
    }
}
