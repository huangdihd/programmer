//! Serializable continuation for opt-in, sequential tool batches.
use super::*;
use crate::tools::ask_user::{QuestionHandler, QuestionKind};
use crate::tools::provider::{ToolApproval, ToolCtx};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PendingKind {
    Review,
    Question,
    Permission,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Pending {
    pub kind: PendingKind,
    pub id: String,
    pub prompt: String,
    pub options: Vec<String>,
    pub call: FunctionToolCall,
    pub reason: Option<String>,
}

/// The conversation already contains every completed output. Only remaining
/// calls live here; save this together with that conversation atomically.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunnerCheckpoint {
    pub pending: Pending,
    pub remaining_calls: Vec<FunctionToolCall>,
    pub steps: usize,
    pub tool_calls: usize,
    pub input_tokens: Option<u32>,
    pub post_edit_reminder_added: bool,
    pub programmer_md_edited: bool,
    pub tool_names: Vec<String>,
    pub before_hooks_done: bool,
    pub edited: bool,
    pub edited_programmer_md: bool,
}

impl RunnerCheckpoint {
    pub(super) fn new(
        calls: Vec<FunctionToolCall>,
        steps: usize,
        tool_calls: usize,
        input_tokens: Option<u32>,
        post_edit_reminder_added: bool,
        programmer_md_edited: bool,
    ) -> Self {
        Self {
            pending: Pending {
                kind: PendingKind::Review,
                id: calls[0].call_id.clone(),
                prompt: String::new(),
                options: vec![],
                call: calls[0].clone(),
                reason: None,
            },
            tool_names: calls.iter().map(|c| c.name.clone()).collect(),
            remaining_calls: calls,
            steps,
            tool_calls,
            input_tokens,
            post_edit_reminder_added,
            programmer_md_edited,
            before_hooks_done: false,
            edited: false,
            edited_programmer_md: false,
        }
    }
}

impl TurnRunner {
    pub(super) async fn run_suspendable_batch(
        &self,
        conversation: &Mutex<Conversation>,
        cancel: &CancellationToken,
        surface: &dyn AgentSurface,
        state: &mut RunnerCheckpoint,
        mut answer: Option<String>,
    ) -> Result<(), RunnerError> {
        if !state.before_hooks_done {
            self.run_hooks(
                hooks::HookPhase::Before,
                conversation,
                &hooks::BatchSummary {
                    tool_names: state.tool_names.clone(),
                    edited: false,
                },
                surface,
                cancel,
            )
            .await?;
            state.before_hooks_done = true;
        }
        while let Some(call) = state.remaining_calls.first().cloned() {
            if cancel.is_cancelled() {
                ensure_tool_output_pairing(conversation);
                return Err(RunnerError::Cancelled);
            }
            let resumed = answer.take();
            let mut output = None;
            let mut tool_answer = None;
            if let Some(answer) = resumed {
                if state.pending.kind == PendingKind::Review {
                    if !matches!(
                        answer.trim().to_ascii_lowercase().as_str(),
                        "yes" | "y" | "approve" | "approved"
                    ) {
                        let reason = answer.strip_prefix("deny:").unwrap_or_else(|| {
                            state.pending.reason.as_deref().unwrap_or("denied by user")
                        });
                        let mut denied = crate::tools::make_tool_output_for_call(
                            &call,
                            Err(format!(
                                "error: tool call denied by user in Manual mode — {reason}"
                            )),
                        );
                        denied.approval_label = Some(format!(
                            "{} denied in Manual mode by user",
                            crate::classifier::WorkMode::Manual.icon()
                        ));
                        output = Some(denied);
                    }
                } else {
                    tool_answer = Some(answer);
                }
            } else if !self.tool_filter.permits(&call.name) {
                output = Some(classify::invalid_tool_call_output(
                    &call,
                    "tool not available in this run",
                ));
            } else if let Err(reason) = self.tools.validate(&call) {
                output = Some(classify::invalid_tool_call_output(&call, &reason));
            } else if self.tools.approval(&call.name, &call.arguments) == ToolApproval::Classify {
                surface.on_event(RunnerEvent::Phase(RunnerPhase::Classifying));
                let outcome = match &self.policy {
                    RunnerPolicy::Yolo => Some(classify::ClassificationOutcome::default()),
                    RunnerPolicy::Sync(classifier) => classify::classify_sync(
                        classifier.as_ref(),
                        std::slice::from_ref(&call),
                        cancel,
                    ),
                    RunnerPolicy::Llm(p) => {
                        let (light, full) = {
                            let conv = conversation.lock().unwrap();
                            classify::build_classifier_context(&conv.items().collect::<Vec<_>>())
                        };
                        classify::classify_llm_with_ask(
                            &crate::classifier::ClassifyContext {
                                client: &p.client,
                                model: &p.model_name,
                                light_context: &light,
                                full_context: &full,
                                top_logprobs: p.top_logprobs,
                            },
                            &p.no_logprobs,
                            vec![call.clone()],
                            cancel,
                            true,
                        )
                        .await
                    }
                };
                let Some(mut outcome) = outcome else {
                    ensure_tool_output_pairing(conversation);
                    return Err(RunnerError::Cancelled);
                };
                output = outcome.denied.pop();
                if let Some((_, reason)) = outcome.ask.pop() {
                    state.pending = Pending {
                        kind: PendingKind::Review,
                        id: uuid::Uuid::new_v4().to_string(),
                        prompt: format!("Approve {}? {reason}", call.name),
                        options: vec!["Approve".into(), "Deny".into()],
                        call,
                        reason: Some(reason),
                    };
                    return Err(RunnerError::InputRequired {
                        checkpoint: Box::new(state.clone()),
                    });
                }
            }
            let output = match output {
                Some(output) => output,
                None => {
                    // Retain the answer sender in the channel until the execution
                    // future is dropped: closing it early would record cancellation.
                    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
                    let tool_answer = Arc::new(Mutex::new(tool_answer));
                    let questions = QuestionHandler::new(move |request| {
                        if let Some(answer) = tool_answer.lock().unwrap().take() {
                            let _ = request.answer.send(answer);
                        } else {
                            let _ = tx.send(request);
                        }
                    });
                    let ctx = ToolCtx {
                        questions: &questions,
                        cancel,
                        operation_id: surface.operation_id(),
                    };
                    surface.on_event(RunnerEvent::Phase(RunnerPhase::RunningTools));
                    let execution = cancel.wait_or(self.tools.call(&call, &ctx));
                    tokio::pin!(execution);
                    let result = tokio::select! {
                        biased;
                        request = rx.recv() => {
                            let request = request.expect("question handler remains alive");
                            let options = match request.question.kind {
                                QuestionKind::Choice { options, .. } => options,
                                QuestionKind::Text => Vec::new(),
                            };
                            state.pending = Pending {
                                kind: if call.name == "request_permission" { PendingKind::Permission } else { PendingKind::Question },
                                id: uuid::Uuid::new_v4().to_string(), prompt: request.question.text, options, call: call.clone(), reason: None,
                            };
                            return Err(RunnerError::InputRequired { checkpoint: Box::new(state.clone()) });
                        }
                        result = &mut execution => result,
                    };
                    let Some(result) = result else {
                        ensure_tool_output_pairing(conversation);
                        return Err(RunnerError::Cancelled);
                    };
                    let mut output = crate::tools::make_tool_output_for_call(&call, result);
                    output.approval_label = Some(surface.approval_label());
                    output
                }
            };
            if !output.failed && is_file_edit_tool(&call.name) {
                state.edited = true;
                state.edited_programmer_md |= edit_targets_programmer_md(&call.arguments);
            }
            conversation.lock().unwrap().add_tool_output(output);
            state.remaining_calls.remove(0);
        }
        self.run_hooks(
            hooks::HookPhase::After,
            conversation,
            &hooks::BatchSummary {
                tool_names: state.tool_names.clone(),
                edited: state.edited,
            },
            surface,
            cancel,
        )
        .await?;
        if state.edited_programmer_md {
            state.programmer_md_edited = true;
            if state.post_edit_reminder_added {
                conversation
                    .lock()
                    .unwrap()
                    .remove_last_developer_message(crate::prompts::POST_EDIT_REMINDER);
                state.post_edit_reminder_added = false;
            }
        }
        if state.edited && !state.programmer_md_edited && !state.post_edit_reminder_added {
            append_post_edit_reminder(conversation);
            state.post_edit_reminder_added = true;
        }
        if let Some(input_tokens) = state.input_tokens {
            surface.usage_safe_point(input_tokens).await?;
        }
        Ok(())
    }
}
