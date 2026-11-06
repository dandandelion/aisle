//! Anthropic Messages API adapter (multipart content, SSE streaming).

use std::time::Duration;

use futures::StreamExt;

use super::openai::truncate;
use super::{sse, ChatMessage, ChatRequest, Provider, ProviderEvent, ProviderStream, Usage};
use crate::config::ProviderSection;
use crate::error::{Error, Result};

pub struct Anthropic {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
    default_model: String,
}

impl Anthropic {
    pub fn new(section: &ProviderSection) -> Result<Self> {
        Ok(Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(300))
                .build()
                .map_err(|e| Error::Config(e.to_string()))?,
            base_url: section
                .base_url
                .clone()
                .unwrap_or_else(|| "https://api.anthropic.com".into())
                .trim_end_matches('/')
                .to_string(),
            api_key: section.api_key.clone(),
            default_model: section.model.clone(),
        })
    }

    /// Anthropic wants system messages as a top-level field and strictly
    /// alternating user/assistant turns (consecutive same-role get merged).
    fn to_wire(messages: &[ChatMessage]) -> (Option<String>, Vec<serde_json::Value>) {
        let mut system: Option<String> = None;
        let mut out: Vec<serde_json::Value> = Vec::new();
        for m in messages {
            if m.role == "system" {
                let t = m.parts.iter().filter_map(|p| p.text()).collect::<Vec<_>>().join("\n");
                system = Some(match system.take() {
                    Some(prev) => format!("{prev}\n{t}"),
                    None => t,
                });
                continue;
            }
            let content: Vec<serde_json::Value> = m
                .parts
                .iter()
                .map(|p| match p {
                    super::ContentPart::Text(t) => serde_json::json!({"type": "text", "text": t}),
                    super::ContentPart::Image { mime, data_base64 } => serde_json::json!({
                        "type": "image",
                        "source": {"type": "base64", "media_type": mime, "data": data_base64}
                    }),
                })
                .collect();
            match out.last_mut() {
                Some(prev)
                    if prev["role"].as_str() == Some(m.role.as_str())
                        && prev["content"].is_array() =>
                {
                    prev["content"]
                        .as_array_mut()
                        .unwrap()
                        .extend(content);
                }
                _ => out.push(serde_json::json!({"role": m.role, "content": content})),
            }
        }
        (system, out)
    }
}

impl Provider for Anthropic {
    fn name(&self) -> &'static str {
        "anthropic"
    }

    fn stream(&self, req: ChatRequest) -> ProviderStream {
        let http = self.http.clone();
        let url = format!("{}/v1/messages", self.base_url);
        let key = self.api_key.clone();
        let model = if req.model.is_empty() { self.default_model.clone() } else { req.model.clone() };
        let (system, messages) = Self::to_wire(&req.messages);

        let mut body = serde_json::json!({
            "model": model,
            "messages": messages,
            "max_tokens": req.max_tokens,
            "stream": true,
        });
        if let Some(sys) = system {
            body["system"] = serde_json::Value::String(sys);
        }

        Box::pin(async_stream::stream! {
            let resp = match http
                .post(&url)
                .header("x-api-key", &key)
                .header("anthropic-version", "2023-06-01")
                .json(&body)
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    yield ProviderEvent::Error { kind: "provider".into(), message: e.to_string() };
                    return;
                }
            };

            let status = resp.status();
            if !status.is_success() {
                let text = resp.text().await.unwrap_or_default();
                yield ProviderEvent::Error {
                    kind: "provider".into(),
                    message: format!("HTTP {status}: {}", truncate(&text, 800)),
                };
                return;
            }

            let mut usage = Usage::default();
            let mut payloads = sse::data_payloads(resp.bytes_stream());
            while let Some(payload) = payloads.next().await {
                let payload = match payload {
                    Ok(p) => p,
                    Err(e) => {
                        yield ProviderEvent::Error { kind: "provider".into(), message: e.to_string() };
                        return;
                    }
                };
                let v: serde_json::Value = match serde_json::from_str(&payload) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                match v.get("type").and_then(|t| t.as_str()).unwrap_or("") {
                    "message_start" => {
                        if let Some(u) = v.get("message").and_then(|m| m.get("usage")) {
                            usage.input_tokens = u.get("input_tokens").and_then(|x| x.as_u64()).unwrap_or(0);
                        }
                    }
                    "content_block_delta" => {
                        let delta = v.get("delta").and_then(|d| d.get("text")).and_then(|t| t.as_str());
                        if let Some(t) = delta {
                            if !t.is_empty() {
                                yield ProviderEvent::TextDelta(t.to_string());
                            }
                        }
                    }
                    "message_delta" => {
                        if let Some(u) = v.get("usage") {
                            usage.output_tokens = u.get("output_tokens").and_then(|x| x.as_u64()).unwrap_or(0);
                        }
                    }
                    "error" => {
                        let msg = v
                            .get("error")
                            .and_then(|e| e.get("message"))
                            .and_then(|m| m.as_str())
                            .unwrap_or("provider error");
                        yield ProviderEvent::Error { kind: "provider".into(), message: msg.into() };
                        return;
                    }
                    _ => {}
                }
            }
            yield ProviderEvent::Done(usage);
        })
    }
}
