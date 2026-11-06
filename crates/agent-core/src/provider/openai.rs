//! OpenAI-compatible adapter: OpenAI, OpenRouter, Ollama /v1, LM Studio, …
//! Chat Completions wire format with SSE streaming.

use std::time::Duration;

use futures::StreamExt;

use super::{sse, ChatMessage, ChatRequest, Provider, ProviderEvent, ProviderStream, Usage};
use crate::config::ProviderSection;
use crate::error::{Error, Result};

pub struct OpenAiCompat {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
    default_model: String,
}

impl OpenAiCompat {
    pub fn new(section: &ProviderSection) -> Result<Self> {
        Ok(Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(300))
                .build()
                .map_err(|e| Error::Config(e.to_string()))?,
            base_url: section
                .base_url
                .clone()
                .unwrap_or_else(|| "https://api.openai.com/v1".into())
                .trim_end_matches('/')
                .to_string(),
            api_key: section.api_key.clone(),
            default_model: section.model.clone(),
        })
    }

    fn to_wire(messages: &[ChatMessage]) -> serde_json::Value {
        messages
            .iter()
            .map(|m| {
                let has_image = m.parts.iter().any(|p| matches!(p, super::ContentPart::Image { .. }));
                let content = if has_image {
                    serde_json::Value::Array(
                        m.parts
                            .iter()
                            .map(|p| match p {
                                super::ContentPart::Text(t) => {
                                    serde_json::json!({"type": "text", "text": t})
                                }
                                super::ContentPart::Image { mime, data_base64 } => {
                                    serde_json::json!({"type": "image_url", "image_url": {
                                        "url": format!("data:{mime};base64,{data_base64}")
                                    }})
                                }
                            })
                            .collect(),
                    )
                } else {
                    serde_json::Value::String(
                        m.parts.iter().filter_map(|p| p.text()).collect::<Vec<_>>().join("\n"),
                    )
                };
                serde_json::json!({"role": m.role, "content": content})
            })
            .collect()
    }
}

impl Provider for OpenAiCompat {
    fn name(&self) -> &'static str {
        "openai"
    }

    fn stream(&self, req: ChatRequest) -> ProviderStream {
        let http = self.http.clone();
        let url = format!("{}/chat/completions", self.base_url);
        let key = self.api_key.clone();
        let model = if req.model.is_empty() { self.default_model.clone() } else { req.model.clone() };

        let body = serde_json::json!({
            "model": model,
            "messages": Self::to_wire(&req.messages),
            "max_tokens": req.max_tokens,
            "stream": true,
            "stream_options": {"include_usage": true},
        });

        Box::pin(async_stream::stream! {
            let resp = match http
                .post(&url)
                .bearer_auth(&key)
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
                if payload == "[DONE]" {
                    break;
                }
                let v: serde_json::Value = match serde_json::from_str(&payload) {
                    Ok(v) => v,
                    Err(_) => continue, // gateway keepalives etc.
                };
                if let Some(u) = v.get("usage") {
                    usage.input_tokens = u.get("prompt_tokens").and_then(|x| x.as_u64()).unwrap_or(0);
                    usage.output_tokens = u.get("completion_tokens").and_then(|x| x.as_u64()).unwrap_or(0);
                }
                if let Some(err) = v.get("error") {
                    yield ProviderEvent::Error {
                        kind: "provider".into(),
                        message: err.get("message").and_then(|m| m.as_str()).unwrap_or("provider error").into(),
                    };
                    return;
                }
                if let Some(delta) = v
                    .get("choices")
                    .and_then(|c| c.get(0))
                    .and_then(|c| c.get("delta"))
                    .and_then(|d| d.get("content"))
                    .and_then(|c| c.as_str())
                {
                    if !delta.is_empty() {
                        yield ProviderEvent::TextDelta(delta.to_string());
                    }
                }
            }
            yield ProviderEvent::Done(usage);
        })
    }
}

pub(crate) fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let mut end = max;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &s[..end])
    }
}
