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

//! The LLM classifier used by Auto mode.
//!
//! Two-stage strategy: a cheap single-token yes/no probe read from logprobs
//! (fast path), falling back to a full reasoned APPROVE/DENY generation when
//! the probe is unavailable, ambiguous, or votes "no".

use super::Verdict;
use crate::prompts::CLASSIFIER_INSTRUCTIONS;
use async_openai::Client;
use async_openai::config::OpenAIConfig;
use async_openai::error::OpenAIError;
use async_openai::types::responses::{
    CreateResponse, IncludeEnum, InputParam, OutputItem, OutputMessageContent,
};

/// Minimum share `P(yes) / (P(yes) + P(no))` the fast (logprob) path requires
/// before it auto-approves — the "safety margin". Anything below falls through
/// to a reasoned deny.
const APPROVE_MARGIN: f64 = 0.7;

/// Output-token ceiling for every classifier call. It is a *ceiling*, not a
/// target: a non-reasoning model answers in one token and stops, so this costs
/// nothing extra there; a reasoning model gets enough room to finish thinking
/// before it emits the answer token whose logprobs we read.
const CLASSIFIER_MAX_TOKENS: u32 = 2048;

/// Number of retries after the initial classifier request for transient
/// transport, rate-limit, and server failures.
const CLASSIFIER_MAX_RETRIES: u32 = 3;

/// The outcome of one LLM classification, plus whether the provider turned out
/// not to support logprobs (so the caller can cache that and skip the fast
/// path next time).
pub struct ClassifyOutcome {
    pub verdict: Verdict,
    pub logprobs_missing: bool,
}

/// Result of the fast single-token yes/no probe.
enum FastResult {
    Approved,
    Denied,
    /// Chosen token wasn't a clear yes/no — fall back to the reasoned path.
    Ambiguous,
    /// Provider returned no logprobs — fall back and remember it.
    NoLogprobs,
}

/// Shared configuration and context for a batch of LLM classifications.
///
/// Bundles the fields that stay constant across every call in a batch (client,
/// model, the two context strings, and the logprobs ceiling) so the per-call
/// function takes one value instead of six.
pub struct ClassifyContext<'a> {
    pub client: &'a Client<OpenAIConfig>,
    pub model: &'a str,
    /// `light_context` (fast path) carries just the working directory and user
    /// request — enough to anchor the yes/no decision without burning tokens on
    /// assistant replies and tool outputs that the single-token probe can't use.
    pub light_context: &'a str,
    /// `full_context` (reasoned fallback) adds assistant replies, tool outputs,
    /// and the recent call history — it is only sent when the fast path couldn't
    /// confidently approve, so the model has the full picture when it re-evaluates
    /// with reasoning.
    pub full_context: &'a str,
    pub top_logprobs: u8,
}

/// Classify one tool call with the LLM.
///
/// `try_logprobs` should be `false` when the model is already known not to
/// support logprobs, so we skip straight to the merged reason-generating call.
pub async fn classify_tool_call(
    ctx: &ClassifyContext<'_>,
    tool_name: &str,
    arguments: &str,
    try_logprobs: bool,
) -> ClassifyOutcome {
    if try_logprobs {
        match classify_fast(
            ctx.client,
            ctx.model,
            tool_name,
            arguments,
            ctx.light_context,
            ctx.top_logprobs,
        )
        .await
        {
            Ok(FastResult::Approved) => {
                return ClassifyOutcome {
                    verdict: Verdict::Allow,
                    logprobs_missing: false,
                };
            }
            Ok(FastResult::Denied) => {
                // Fast path voted "no" — re-evaluate with full context and
                // reasoning so the model gets a chance to approve after seeing
                // the bigger picture, reducing false positives.
                return ClassifyOutcome {
                    verdict: classify_reasoned(
                        ctx.client,
                        ctx.model,
                        tool_name,
                        arguments,
                        ctx.full_context,
                    )
                    .await,
                    logprobs_missing: false,
                };
            }
            Ok(FastResult::Ambiguous) => {
                return ClassifyOutcome {
                    verdict: classify_reasoned(
                        ctx.client,
                        ctx.model,
                        tool_name,
                        arguments,
                        ctx.full_context,
                    )
                    .await,
                    logprobs_missing: false,
                };
            }
            Ok(FastResult::NoLogprobs) => {
                return ClassifyOutcome {
                    verdict: classify_reasoned(
                        ctx.client,
                        ctx.model,
                        tool_name,
                        arguments,
                        ctx.full_context,
                    )
                    .await,
                    logprobs_missing: true,
                };
            }
            Err(e) => {
                return ClassifyOutcome {
                    verdict: Verdict::Deny {
                        reason: format!("classifier error: {e}"),
                    },
                    logprobs_missing: false,
                };
            }
        }
    }

    // Known to lack logprobs: go straight to the merged path.
    ClassifyOutcome {
        verdict: classify_reasoned(
            ctx.client,
            ctx.model,
            tool_name,
            arguments,
            ctx.full_context,
        )
        .await,
        logprobs_missing: true,
    }
}

/// Assemble the per-call prompt body: optional turn context followed by the
/// tool name and arguments under review.
fn call_block(context: &str, tool_name: &str, arguments: &str) -> String {
    if context.trim().is_empty() {
        format!("Tool: {tool_name}\nArguments:\n{arguments}")
    } else {
        format!("Context:\n{context}\n\nTool: {tool_name}\nArguments:\n{arguments}")
    }
}

/// Build a base classifier request (non-streaming, deterministic, no tools).
fn base_request(model: &str, prompt: String) -> CreateResponse {
    CreateResponse {
        model: Some(model.to_string()),
        input: InputParam::Text(prompt),
        instructions: Some(CLASSIFIER_INSTRUCTIONS.to_string()),
        reasoning: Some(async_openai::types::responses::ReasoningEffort::None.into()),
        temperature: Some(0.0),
        store: Some(false),
        ..Default::default()
    }
}

fn fast_request(model: &str, prompt: String, top_logprobs: u8) -> CreateResponse {
    let mut request = base_request(model, prompt);
    request.max_output_tokens = Some(CLASSIFIER_MAX_TOKENS);
    request.top_logprobs = Some(top_logprobs);
    request.include = Some(vec![IncludeEnum::MessageOutputTextLogprobs]);
    request
}

/// Pull the first assistant text out of a response, with its logprobs.
fn first_text(
    response: &async_openai::types::responses::Response,
) -> Option<&OutputMessageContent> {
    response.output.iter().find_map(|item| match item {
        OutputItem::Message(msg) => msg.content.first(),
        _ => None,
    })
}

/// Send a non-streaming classifier request with the same transient-error
/// policy as normal model streams. Classifier calls are short and have no
/// partial output to reconcile, so replaying the whole request is safe.
async fn create_with_retries(
    client: &Client<OpenAIConfig>,
    request: &CreateResponse,
) -> Result<async_openai::types::responses::Response, OpenAIError> {
    let mut attempt = 0;
    loop {
        match client.responses().create(request.clone()).await {
            Ok(response) => return Ok(response),
            Err(error)
                if crate::runner::stream::is_retryable(&error)
                    && attempt < CLASSIFIER_MAX_RETRIES =>
            {
                attempt += 1;
                tokio::time::sleep(crate::runner::stream::backoff_delay(attempt)).await;
            }
            Err(error) => return Err(error),
        }
    }
}

/// Fast path: make the model answer yes/no, then compare `P(yes)` vs `P(no)` in
/// the logprobs of the *first message-content token*.
///
/// We deliberately do NOT cap at one token. A reasoning model spends its first
/// tokens on a separate `reasoning` output item — the answer only appears once
/// `message` content begins, and reasoning logprobs never land in
/// `message.output_text`. So the first content token is always the yes/no
/// answer, for reasoning and non-reasoning models alike.
async fn classify_fast(
    client: &Client<OpenAIConfig>,
    model: &str,
    tool_name: &str,
    arguments: &str,
    context: &str,
    top_logprobs: u8,
) -> Result<FastResult, String> {
    let prompt = format!(
        "{}\n\nShould this tool call be auto-approved? \
         Consider the instructions, the context, and any explicit user \
         direction. Answer with exactly one word: yes or no.",
        call_block(context, tool_name, arguments)
    );
    let req = fast_request(model, prompt, top_logprobs);

    let response = match create_with_retries(client, &req).await {
        Ok(r) => r,
        // Some providers return logprobs in a non-spec shape (e.g. omitting
        // the `bytes` field on empty tokens), which fails response
        // deserialization even though the request itself succeeded. Treat
        // that as "logprobs unavailable" so the caller falls back to the
        // reasoned path and caches the provider as logprobs-less, instead
        // of denying the tool call outright.
        Err(OpenAIError::JSONDeserialize(..)) => return Ok(FastResult::NoLogprobs),
        Err(e) => return Err(e.to_string()),
    };

    let Some(OutputMessageContent::OutputText(text)) = first_text(&response) else {
        return Ok(FastResult::Ambiguous);
    };
    let Some(logprobs) = &text.logprobs else {
        return Ok(FastResult::NoLogprobs);
    };
    let Some(first) = logprobs.first() else {
        return Ok(FastResult::NoLogprobs);
    };

    // Candidate set is the chosen token plus its alternatives.
    let mut p_yes = 0.0_f64;
    let mut p_no = 0.0_f64;
    let chosen = std::iter::once((first.token.as_str(), first.logprob));
    let alts = first
        .top_logprobs
        .iter()
        .map(|t| (t.token.as_str(), t.logprob));
    for (token, logprob) in chosen.chain(alts) {
        match yes_no_bucket(token) {
            Some(true) => p_yes += logprob.exp(),
            Some(false) => p_no += logprob.exp(),
            None => {}
        }
    }

    if p_yes == 0.0 && p_no == 0.0 {
        return Ok(FastResult::Ambiguous);
    }
    let share_yes = p_yes / (p_yes + p_no);
    if share_yes >= APPROVE_MARGIN {
        Ok(FastResult::Approved)
    } else {
        Ok(FastResult::Denied)
    }
}

/// Normalise a token to yes (`Some(true)`), no (`Some(false)`), or neither.
fn yes_no_bucket(token: &str) -> Option<bool> {
    let t = token.trim().trim_matches(|c: char| !c.is_alphanumeric());
    match t.to_ascii_lowercase().as_str() {
        "yes" | "y" => Some(true),
        "no" | "n" => Some(false),
        _ => None,
    }
}

/// Merged path: decide and explain in a single generation. Used when the model
/// lacks logprobs, when the fast token was ambiguous, or when the fast path
/// voted "no" and the model gets a chance to re-evaluate with full context.
async fn classify_reasoned(
    client: &Client<OpenAIConfig>,
    model: &str,
    tool_name: &str,
    arguments: &str,
    context: &str,
) -> Verdict {
    let prompt = format!(
        "{}\n\nDecide whether this tool call should be auto-approved. \
         Consider the instructions, the context, and any explicit user \
         direction. Reply on a single line, exactly one of:\n\
         APPROVE\n\
         DENY: <one-sentence reason>",
        call_block(context, tool_name, arguments)
    );
    let mut req = base_request(model, prompt);
    req.max_output_tokens = Some(CLASSIFIER_MAX_TOKENS);

    let response = match create_with_retries(client, &req).await {
        Ok(r) => r,
        Err(e) => {
            return Verdict::Deny {
                reason: format!("classifier error: {e}"),
            };
        }
    };

    let text = match first_text(&response) {
        Some(OutputMessageContent::OutputText(t)) => t.text.trim().to_string(),
        _ => String::new(),
    };
    parse_reasoned(&text)
}

/// Parse the merged path's reply into a verdict.
fn parse_reasoned(text: &str) -> Verdict {
    let trimmed = text.trim();
    let upper = trimmed.to_ascii_uppercase();
    if upper.starts_with("APPROVE") {
        return Verdict::Allow;
    }
    // Everything else is treated as a denial; extract the reason after "DENY".
    let reason = trimmed
        .strip_prefix("DENY")
        .or_else(|| trimmed.strip_prefix("deny"))
        .map(|r| r.trim_start_matches([':', '-', ' ']).trim())
        .filter(|r| !r.is_empty())
        .map(|r| r.to_string())
        .unwrap_or_else(|| {
            if trimmed.is_empty() {
                "classifier returned no decision".to_string()
            } else {
                trimmed.to_string()
            }
        });
    Verdict::Deny { reason }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifier_requests_disable_reasoning() {
        let request = base_request("test-model", "test prompt".to_string());
        let value = serde_json::to_value(request).expect("classifier request should serialize");

        assert_eq!(value["reasoning"]["effort"], "none");
    }

    #[test]
    fn fast_request_uses_configured_top_logprobs() {
        let request = fast_request("test-model", "test prompt".to_string(), 5);
        let value = serde_json::to_value(request).expect("fast request should serialize");

        assert_eq!(value["top_logprobs"], 5);
    }

    #[test]
    fn parse_reasoned_verdicts() {
        assert!(matches!(parse_reasoned("APPROVE"), Verdict::Allow));
        assert!(matches!(
            parse_reasoned("DENY: too risky"),
            Verdict::Deny { .. }
        ));
        let reason = match parse_reasoned("DENY: too risky") {
            Verdict::Deny { reason } => reason,
            _ => panic!(),
        };
        assert_eq!(reason, "too risky");
    }

    #[test]
    fn yes_no_bucketing() {
        assert_eq!(yes_no_bucket("yes"), Some(true));
        assert_eq!(yes_no_bucket("Yes"), Some(true));
        assert_eq!(yes_no_bucket("no"), Some(false));
        assert_eq!(yes_no_bucket("No"), Some(false));
        assert_eq!(yes_no_bucket(""), None);
        assert_eq!(yes_no_bucket("maybe"), None);
    }
}
