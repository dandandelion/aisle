//! Contract tests: exact outbound payload + SSE parsing against a mock
//! "OpenAI-compatible" and "Anthropic" server. These are the highest-value
//! tests in the project (plan.md test strategy).

use std::io::{Read, Write};
use std::net::TcpListener;

use agent_core::config::ProviderSection;
use agent_core::provider::{self, ChatMessage, ChatRequest, ContentPart, Provider, ProviderEvent};
use futures::StreamExt;

/// Tiny single-shot HTTP server that replies with a canned SSE body and
/// records the request. Returns (base_url, handle-to-request-body).
fn mock_sse_server(status_line: &str, headers: &[(&str, &str)], sse_body: &str) -> (String, std::sync::mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let body = sse_body.to_string();
    let status_line = status_line.to_string();
    let hdrs: Vec<(String, String)> = headers.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    std::thread::spawn(move || {
        let (mut sock, _) = listener.accept().unwrap();
        let mut buf = vec![0u8; 64 * 1024];
        let mut acc = Vec::new();
        loop {
            let n = sock.read(&mut buf).unwrap();
            if n == 0 {
                break;
            }
            acc.extend_from_slice(&buf[..n]);
            if acc.windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
        }
        // read declared body if present
        let head = String::from_utf8_lossy(&acc).to_string();
        let clen: usize = head
            .lines()
            .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse().unwrap_or(0)))
            .unwrap_or(0);
        let have = acc.len() - acc.windows(4).position(|w| w == b"\r\n\r\n").unwrap() - 4;
        let mut need = clen.saturating_sub(have);
        while need > 0 {
            let cap = buf.len();
            let n = sock.read(&mut buf[..need.min(cap)]).unwrap();
            if n == 0 {
                break;
            }
            acc.extend_from_slice(&buf[..n]);
            need -= n;
        }
        tx.send(String::from_utf8_lossy(&acc).into_owned()).ok();

        let mut resp = format!("HTTP/1.1 {status_line}\r\n");
        for (k, v) in &hdrs {
            resp.push_str(&format!("{k}: {v}\r\n"));
        }
        resp.push_str(&format!("content-length: {}\r\n\r\n{body}", body.len()));
        sock.write_all(resp.as_bytes()).unwrap();
        sock.flush().ok();
    });
    (format!("http://{addr}"), rx)
}

fn section(kind: &str, base_url: String) -> ProviderSection {
    ProviderSection {
        kind: kind.into(),
        base_url: Some(base_url),
        model: "test-model".into(),
        api_key: "test-key".into(),
        named: Default::default(),
    }
}

async fn collect(p: &dyn Provider, req: ChatRequest) -> (String, Vec<(String, String)>) {
    let mut text = String::new();
    let mut events = Vec::new();
    let mut stream = p.stream(req);
    while let Some(ev) = stream.next().await {
        match ev {
            ProviderEvent::TextDelta(t) => text.push_str(&t),
            ProviderEvent::Done(u) => events.push(("done".into(), format!("{}|{}", u.input_tokens, u.output_tokens))),
            ProviderEvent::Error { kind, message } => events.push((kind, message)),
            ProviderEvent::ToolCall { .. } => events.push(("tool".into(), String::new())),
        }
    }
    (text, events)
}

fn req_with_image() -> ChatRequest {
    ChatRequest {
        model: String::new(), // provider default
        messages: vec![
            ChatMessage { role: "system".into(), parts: vec![ContentPart::Text("sys".into())] },
            ChatMessage {
                role: "user".into(),
                parts: vec![
                    ContentPart::Text("what is this".into()),
                    ContentPart::Image { mime: "image/jpeg".into(), data_base64: "QUJD".into() },
                ],
            },
        ],
        max_tokens: 128,
    }
}

#[tokio::test]
async fn openai_compat_wire_and_stream() {
    let sse = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"lo\"}}]}\n\n",
        "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":11,\"completion_tokens\":2}}\n\n",
        "data: [DONE]\n\n",
    );
    let (base, rx) = mock_sse_server(
        "200 OK",
        &[("content-type", "text/event-stream")],
        sse,
    );
    let p = provider::openai::OpenAiCompat::new(&section("openai", base)).unwrap();
    let (text, events) = collect(&p, req_with_image()).await;
    assert_eq!(text, "Hello");
    assert_eq!(events, vec![("done".into(), "11|2".into())]);

    let request = rx.recv().unwrap();
    assert!(request.contains("POST /chat/completions HTTP/1.1"));
    assert!(request.to_ascii_lowercase().contains("authorization: bearer test-key"));
    // body assertions
    let body = request.split("\r\n\r\n").nth(1).unwrap();
    let v: serde_json::Value = serde_json::from_str(body).unwrap();
    assert_eq!(v["model"], "test-model");
    assert_eq!(v["stream"], true);
    assert_eq!(v["messages"][0]["role"], "system");
    assert_eq!(v["messages"][0]["content"], "sys"); // plain string when no images
    assert!(v["messages"][1]["content"].is_array());
    assert_eq!(v["messages"][1]["content"][1]["type"], "image_url");
    assert_eq!(
        v["messages"][1]["content"][1]["image_url"]["url"],
        "data:image/jpeg;base64,QUJD"
    );
}

#[tokio::test]
async fn openai_compat_error_body_surfaces() {
    let (base, _rx) = mock_sse_server(
        "401 Unauthorized",
        &[("content-type", "application/json")],
        r#"{"error":{"message":"invalid api key"}}"#,
    );
    let p = provider::openai::OpenAiCompat::new(&section("openai", base)).unwrap();
    let (_, events) = collect(&p, req_with_image()).await;
    assert!(matches!(&events[..], [(k, m)] if k == "provider" && m.contains("401") && m.contains("invalid api key")), "{events:?}");
}

#[tokio::test]
async fn anthropic_wire_and_stream() {
    let sse = concat!(
        "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":7}}}\n\n",
        "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"Hi\"}}\n\n",
        "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\" there\"}}\n\n",
        "data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":2}}\n\n",
        "data: {\"type\":\"message_stop\"}\n\n",
    );
    let (base, rx) = mock_sse_server("200 OK", &[("content-type", "text/event-stream")], sse);
    let p = provider::anthropic::Anthropic::new(&section("anthropic", base)).unwrap();
    let (text, events) = collect(&p, req_with_image()).await;
    assert_eq!(text, "Hi there");
    assert_eq!(events, vec![("done".into(), "7|2".into())]);

    let request = rx.recv().unwrap();
    assert!(request.contains("POST /v1/messages HTTP/1.1"));
    assert!(request.to_ascii_lowercase().contains("x-api-key: test-key"));
    assert!(request.to_ascii_lowercase().contains("anthropic-version: 2023-06-01"));
    let body = request.split("\r\n\r\n").nth(1).unwrap();
    let v: serde_json::Value = serde_json::from_str(body).unwrap();
    assert_eq!(v["system"], "sys"); // system extracted to top level
    assert_eq!(v["messages"].as_array().unwrap().len(), 1);
    assert_eq!(v["messages"][0]["content"][1]["type"], "image");
    assert_eq!(v["messages"][0]["content"][1]["source"]["type"], "base64");
}
