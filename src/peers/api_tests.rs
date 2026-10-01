use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn question() -> PeerEnvelope {
    PeerEnvelope::new(
        uuid::Uuid::new_v4().to_string(),
        uuid::Uuid::new_v4().to_string(),
        PeerKind::Question,
        "What did you find?".into(),
        None,
    )
    .unwrap()
}

/// Consume the entire Content-Length body, including bodies split across reads.
async fn read_request(socket: &mut tokio::net::TcpStream) -> Value {
    let mut bytes = Vec::new();
    let mut chunk = [0; 4096];
    loop {
        if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
            let headers = std::str::from_utf8(&bytes[..end]).unwrap();
            assert!(headers.starts_with("POST /v1/responses HTTP/1.1\r\n"));
            let length = headers
                .lines()
                .filter_map(|line| line.split_once(':'))
                .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                .expect("request must have Content-Length")
                .1
                .trim()
                .parse::<usize>()
                .unwrap();
            let start = end + 4;
            if bytes.len() >= start + length {
                return serde_json::from_slice(&bytes[start..start + length]).unwrap();
            }
        }
        let count = socket.read(&mut chunk).await.unwrap();
        assert_ne!(count, 0, "connection closed before request body arrived");
        bytes.extend_from_slice(&chunk[..count]);
    }
}

async fn mock_answer(
    question: &PeerEnvelope,
    output: Value,
) -> (Result<PeerEnvelope, String>, Vec<Value>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = Client::with_config(
        OpenAIConfig::new()
            .with_api_base(format!("http://{}/v1", listener.local_addr().unwrap()))
            .with_api_key("test-only"),
    );
    let body = json!({
        "id": "resp_peer_answer",
        "object": "response",
        "created_at": 0,
        "model": "mock-peer-model",
        "status": "completed",
        "output": output,
        "usage": {
            "input_tokens": 10,
            "input_tokens_details": {"cached_tokens": 0},
            "output_tokens": 5,
            "output_tokens_details": {"reasoning_tokens": 0},
            "total_tokens": 15
        }
    })
    .to_string();
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut requests = Vec::new();
    // Keep serving until answer_question returns, so an accidental second API
    // call is recorded rather than hanging against a one-request mock.
    let result = {
        let server = async {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                requests.push(read_request(&mut socket).await);
                socket.write_all(response.as_bytes()).await.unwrap();
                socket.shutdown().await.unwrap();
            }
        };
        tokio::select! {
            result = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                answer_question(question, &[], &client, "mock-peer-model"),
            ) => result.expect("peer answer did not finish within five seconds"),
            _ = server => unreachable!("mock server runs until the answer completes"),
        }
    };
    (result, requests)
}

fn assert_one_tool_free_request(requests: &[Value], question: &PeerEnvelope) {
    assert_eq!(
        requests.len(),
        1,
        "peer answers must use exactly one API call"
    );
    let request = &requests[0];
    assert!(request.get("tools").is_none(), "tools must be omitted");
    assert!(request.get("tool_choice").is_none());
    assert_eq!(request["model"], "mock-peer-model");
    assert_eq!(request["stream"], false);
    assert_eq!(request["store"], false);
    assert!(
        request.get("max_output_tokens").is_none(),
        "peer answers must use the normal Responses output-token default"
    );
    // A text-only input cannot contain function calls or function-call results.
    let input: Value = serde_json::from_str(request["input"].as_str().unwrap()).unwrap();
    assert_eq!(input["from_session"], question.from);
    assert_eq!(input["to_session"], question.to);
    assert_eq!(input["question"], question.body);
}

#[tokio::test]
async fn answer_question_makes_one_tool_free_request_and_preserves_exchange_identity() {
    let question = question();
    let (result, requests) = mock_answer(
        &question,
        json!([{
            "id": "msg_peer_answer",
            "type": "message",
            "role": "assistant",
            "status": "completed",
            "content": [
                {"type": "output_text", "text": "Found the cause.", "annotations": []},
                {"type": "output_text", "text": "Add a regression test.", "annotations": []}
            ]
        }]),
    )
    .await;
    assert_one_tool_free_request(&requests, &question);
    let mut expected = question.clone();
    expected.kind = PeerKind::Exchange;
    expected.answer = Some("Found the cause.\nAdd a regression test.".into());
    assert_eq!(result.unwrap(), expected);
}

#[tokio::test]
async fn answer_question_rejects_non_text_output_without_following_tool_calls() {
    let question = question();
    let (result, requests) = mock_answer(
        &question,
        json!([{
            "type": "function_call",
            "id": "fc_unexpected",
            "call_id": "call_unexpected",
            "name": "command",
            "arguments": "{\"command\":\"must not run\"}",
            "status": "completed"
        }]),
    )
    .await;
    assert_one_tool_free_request(&requests, &question);
    assert_eq!(result.unwrap_err(), "Peer returned no textual answer");
}
