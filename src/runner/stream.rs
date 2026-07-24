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

//! The shared streaming primitive: open a streaming response with bounded
//! retries and deliver each event (or a terminal error) to a caller-supplied
//! sink. Both the TUI (sink = send an `AppEvent` on the channel) and the
//! headless runner (sink = fold a [`PartialResponse`] directly) drive their
//! requests through this one function so retry and cancellation semantics can
//! never drift between the two paths.

use crate::cancel::CancellationToken;
use async_openai::Client;
use async_openai::config::OpenAIConfig;
use async_openai::error::OpenAIError;
use async_openai::types::responses::{CreateResponse, ResponseStreamEvent};
use futures::StreamExt;
use std::sync::atomic::{AtomicBool, Ordering};

/// Whether a failed `create_stream` is worth retrying: transport-level errors
/// with no HTTP status (connection refused, DNS, reset), plus transient server
/// responses (429 rate-limit and 5xx gateway/overload codes).
pub(crate) fn is_retryable(error: &OpenAIError) -> bool {
    match error {
        OpenAIError::Reqwest(e) => match e.status() {
            None => true,
            Some(status) => {
                status.as_u16() == 429
                    || matches!(status.as_u16(), 500 | 502 | 503 | 504)
            }
        },
        _ => false,
    }
}

/// Exponential backoff for retry `attempt` (1-based): `2^(attempt-1)` seconds
/// capped at 30s, plus up to ~500ms of jitter to avoid synchronized retries.
pub(crate) fn backoff_delay(attempt: u32) -> std::time::Duration {
    const CAP_SECS: u64 = 30;
    let base = 1u64.checked_shl(attempt - 1).unwrap_or(CAP_SECS).min(CAP_SECS);
    let jitter_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| (d.subsec_nanos() as u64) % 500)
        .unwrap_or(0);
    std::time::Duration::from_secs(base) + std::time::Duration::from_millis(jitter_ms)
}

/// Open `request` as a streaming response and pump every event into `sink`.
///
/// Retries the initial connection on transient failures (up to
/// [`crate::consts::MAX_STREAM_RETRIES`]) with exponential backoff, flipping
/// `retrying` true only while a backoff is pending. Both a terminal connection
/// error and any per-event error are delivered to `sink` as `Err`; successful
/// events arrive as `Ok`. Once `cancel` is cancelled the function stops
/// silently — no further sink calls, no error surfaced — so a cancelled turn
/// never emits a late event or a spurious error.
pub(crate) async fn stream_with_retries(
    client: &Client<OpenAIConfig>,
    request: &CreateResponse,
    cancel: &CancellationToken,
    retrying: &AtomicBool,
    mut sink: impl FnMut(Result<ResponseStreamEvent, OpenAIError>),
) {
    retrying.store(false, Ordering::Relaxed);
    let mut attempt: u32 = 0;
    let stream = loop {
        match client.responses().create_stream(request.clone()).await {
            Ok(stream) => break Ok(stream),
            Err(e) if is_retryable(&e) && attempt < crate::consts::MAX_STREAM_RETRIES => {
                if cancel.is_cancelled() {
                    retrying.store(false, Ordering::Relaxed);
                    return;
                }
                attempt += 1;
                retrying.store(true, Ordering::Relaxed);
                tokio::time::sleep(backoff_delay(attempt)).await;
            }
            Err(e) => break Err(e),
        }
    };
    retrying.store(false, Ordering::Relaxed);
    match stream {
        Ok(mut response_stream) => {
            while let Some(response_stream_event) = response_stream.next().await {
                if cancel.is_cancelled() {
                    return;
                }
                sink(response_stream_event);
            }
        }
        Err(openai_error) => {
            if !cancel.is_cancelled() {
                sink(Err(openai_error));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_exponentially_and_caps() {
        // Base component (ignoring the sub-second jitter): 2^(n-1) seconds,
        // capped at 30s.
        assert_eq!(backoff_delay(1).as_secs(), 1);
        assert_eq!(backoff_delay(2).as_secs(), 2);
        assert_eq!(backoff_delay(3).as_secs(), 4);
        assert_eq!(backoff_delay(6).as_secs(), 30, "2^5 = 32 → capped at 30s");
        // Very large attempts saturate to the cap rather than overflow.
        assert_eq!(backoff_delay(64).as_secs(), 30);
        // Jitter stays under 500ms.
        assert!(backoff_delay(1).subsec_millis() < 500);
    }
}
