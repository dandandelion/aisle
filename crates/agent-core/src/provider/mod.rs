//! Provider abstraction (spec §8 "Agent core interfaces").
//! One trait, event-stream shaped, so tool calls can arrive later without
//! protocol churn (ADR-0008).

pub mod anthropic;
pub mod openai;
mod sse;

use futures::stream::BoxStream;

use crate::config::ProviderSection;

#[derive(Debug, Clone)]
pub enum ContentPart {
    Text(String),
    Image { mime: String, data_base64: String },
}

impl ContentPart {
    pub fn text(&self) -> Option<&str> {
        match self {
            ContentPart::Text(t) => Some(t),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub role: String, // user | assistant | system
    pub parts: Vec<ContentPart>,
}

#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub max_tokens: u32,
}

#[derive(Debug, Clone, Default)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

#[derive(Debug, Clone)]
pub enum ProviderEvent {
    TextDelta(String),
    /// Reserved for the future tool registry (ADR-0008). Never emitted in v1.
    ToolCall { id: String, name: String, input: serde_json::Value },
    Done(Usage),
    Error { kind: String, message: String },
}

pub type ProviderStream = BoxStream<'static, ProviderEvent>;

pub trait Provider: Send + Sync {
    fn name(&self) -> &'static str;
    fn stream(&self, req: ChatRequest) -> ProviderStream;
}

pub fn build(section: &ProviderSection) -> crate::error::Result<std::sync::Arc<dyn Provider>> {
    use std::sync::Arc;
    match section.kind.as_str() {
        "openai" => Ok(Arc::new(openai::OpenAiCompat::new(section)?)),
        "anthropic" => Ok(Arc::new(anthropic::Anthropic::new(section)?)),
        other => Err(crate::error::Error::Config(format!("unknown provider kind {other:?}"))),
    }
}

/// Test-only deterministic provider.
pub struct MockProvider {
    pub reply: String,
    pub chunk_every: usize,
}

impl Provider for MockProvider {
    fn name(&self) -> &'static str {
        "mock"
    }
    fn stream(&self, _req: ChatRequest) -> ProviderStream {
        let reply = self.reply.clone();
        let every = self.chunk_every.max(1);
        let mut evs: Vec<ProviderEvent> = reply
            .chars()
            .collect::<Vec<_>>()
            .chunks(every)
            .map(|c| ProviderEvent::TextDelta(c.iter().collect()))
            .collect();
        evs.push(ProviderEvent::Done(Usage::default()));
        Box::pin(futures::stream::iter(evs))
    }
}
