//! Context assembly: DB rows + attachments -> provider ChatRequests, with
//! the spec §9.3 image policy (last N images kept, older images ship
//! text-only) as a tested invariant.

use crate::config::ImageSection;
use crate::provider::{ChatMessage, ChatRequest, ContentPart};
use crate::store::Msg;

pub const SYSTEM_PROMPT: &str = "\
You are sideagent, a concise assistant available as an overlay on the user's \
Linux desktop. When a screenshot is attached, it shows the user's current \
screen; reference visible content directly. Be brief; use markdown for code. \
If a question does not need the screen, answer from knowledge.";

/// Per-request answer styles (side-ask --brief / --explain). Injected into
/// the system message of *this* request only — never persisted, so other
/// frontends see the plain question in history.
pub fn style_note(style: &str) -> Option<&'static str> {
    match style {
        "brief" => Some("Answer style — BRIEF: output ONLY the answer, at most 2 \
            lines. If the answer is a command, output just the command in a code \
            fence and nothing else. No preamble, no explanation, no alternatives, \
            no caveats (omit even pushed-commits warnings)."),
        "explain" => Some("Answer style — TEACH: lead with the direct answer in a \
            code fence, then a compact explanation of the key parts, then one \
            memorable tip (mnemonic or analogy) for recalling it. Keep it tight."),
        _ => None,
    }
}

pub fn build_request(
    model: &str,
    history: &[Msg],
    images: &[(usize, PreparedImage)], // (message index in history, prepared image)
    image_cfg: &ImageSection,
    style: Option<&str>,
    context: Option<&str>,
    max_tokens: u32,
) -> ChatRequest {
    let keep_from = images.len().saturating_sub(image_cfg.keep_images_last);
    let keep: std::collections::HashSet<usize> =
        images[keep_from..].iter().map(|(i, _)| *i).collect();

    let mut system = SYSTEM_PROMPT.to_string();
    if let Some(note) = style.and_then(style_note) {
        system.push('\n');
        system.push_str(note);
    }
    if let Some(ctx) = context.filter(|c| !c.trim().is_empty()) {
        system.push_str("\nUser runtime context for this question:\n");
        system.push_str(ctx.trim_end());
    }
    let mut messages = vec![ChatMessage {
        role: "system".into(),
        parts: vec![ContentPart::Text(system)],
    }];

    for (idx, m) in history.iter().enumerate() {
        let mut parts = vec![ContentPart::Text(m.text.clone())];
        if let Some((_, img)) = images.iter().find(|(i, _)| *i == idx) {
            if keep.contains(&idx) {
                parts.push(ContentPart::Image {
                    mime: img.mime.clone(),
                    data_base64: img.base64.clone(),
                });
            } else {
                parts.push(ContentPart::Text(format!(
                    "[screenshot {}x{} omitted from context: older images are dropped]",
                    img.width, img.height
                )));
            }
        }
        messages.push(ChatMessage { role: m.role.clone(), parts });
    }

    ChatRequest { model: model.into(), messages, max_tokens }
}

/// An image that has passed the pipeline and is ready for the wire.
#[derive(Debug, Clone)]
pub struct PreparedImage {
    pub mime: String,
    pub base64: String,
    pub width: u32,
    pub height: u32,
    pub bytes: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(role: &str, text: &str) -> Msg {
        Msg { id: 0, session_id: "s".into(), role: role.into(), text: role_text(text), created_at: String::new() }
    }
    fn role_text(t: &str) -> String {
        t.to_string()
    }

    fn img(w: u32) -> PreparedImage {
        PreparedImage { mime: "image/jpeg".into(), base64: format!("B64{w}"), width: w, height: w, bytes: 100 }
    }

    #[test]
    fn keeps_only_last_two_images() {
        let history = vec![
            msg("user", "q1"),
            msg("user", "q2"),
            msg("user", "q3"),
            msg("user", "q4"),
        ];
        let images = vec![(0, img(1)), (1, img(2)), (2, img(3))];
        let req = build_request("m", &history, &images, &ImageSection::default(), None, None, 8192);
        // system + 4 user
        assert_eq!(req.messages.len(), 5);
        let img_count = req
            .messages
            .iter()
            .flat_map(|m| m.parts.iter())
            .filter(|p| matches!(p, ContentPart::Image { .. }))
            .count();
        assert_eq!(img_count, 2);
        // the kept ones are the two most recent: indexes 1 and 2
        let kept: Vec<String> = req
            .messages
            .iter()
            .flat_map(|m| m.parts.iter())
            .filter_map(|p| match p {
                ContentPart::Image { data_base64, .. } => Some(data_base64.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(kept, vec!["B642", "B643"]);
        // dropped image leaves a marker in its text message
        assert!(req.messages[1].parts.iter().any(|p| matches!(p, ContentPart::Text(t) if t.contains("omitted"))));
    }

    #[test]
    fn no_images_is_just_text() {
        let history = vec![msg("user", "hello"), msg("assistant", "hi")];
        let req = build_request("m", &history, &[], &ImageSection::default(), None, None, 8192);
        assert_eq!(req.messages.len(), 3); // + system
    }

    #[test]
    fn cap_flows_to_request() {
        let history = vec![msg("user", "hi")];
        let req = build_request("m", &history, &[], &ImageSection::default(), None, None, 16384);
        assert_eq!(req.max_tokens, 16384);
    }

    #[test]
    fn runtime_context_lands_in_system_only() {
        let history = vec![msg("user", "list the files here")];
        let req = build_request("m", &history, &[], &ImageSection::default(),
            Some("brief"), Some("working directory: /tmp/proj"), 8192);
        let ContentPart::Text(sys) = &req.messages[0].parts[0] else { panic!("system first") };
        assert!(sys.contains("working directory: /tmp/proj"));
        assert!(sys.contains("BRIEF"));
        let ContentPart::Text(u) = &req.messages[1].parts[0] else { unreachable!() };
        assert_eq!(u, "list the files here");
    }

    #[test]
    fn style_note_appends_to_system_only() {
        let history = vec![msg("user", "hello")];
        let req = build_request("m", &history, &[], &ImageSection::default(), Some("brief"), None, 8192);
        let ContentPart::Text(sys) = &req.messages[0].parts[0] else { panic!("system first") };
        assert!(sys.contains("BRIEF"));
        let ContentPart::Text(u) = &req.messages[1].parts[0] else { panic!("user text") };
        assert_eq!(u, "hello"); // question untouched
        // unknown style == no style
        let req2 = build_request("m", &history, &[], &ImageSection::default(), Some("weird"), None, 8192);
        let ContentPart::Text(sys2) = &req2.messages[0].parts[0] else { unreachable!() };
        assert!(!sys2.contains("BRIEF"));
    }
}
