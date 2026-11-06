//! Minimal SSE line splitter: yields the payload of each `data:` line from a
//! chunked byte stream. Shared by both provider adapters.

use futures::StreamExt;

pub type SseItem = std::result::Result<String, reqwest::Error>;

/// Turn a stream of byte chunks into a stream of `data:` payload strings.
pub fn data_payloads<B, S>(
    chunks: S,
) -> futures::stream::BoxStream<'static, SseItem>
where
    B: AsRef<[u8]> + Send + 'static,
    S: futures::Stream<Item = std::result::Result<B, reqwest::Error>> + Send + 'static,
{
    Box::pin(external_stream(chunks))
}

fn external_stream<B, S>(chunks: S) -> impl futures::Stream<Item = SseItem> + Send
where
    B: AsRef<[u8]> + Send,
    S: futures::Stream<Item = std::result::Result<B, reqwest::Error>> + Send + 'static,
{
    async_stream::stream! {
        let mut carry: Vec<u8> = Vec::new();
        let mut chunks = std::pin::pin!(chunks);
        while let Some(chunk) = chunks.next().await {
            match chunk {
                Ok(b) => carry.extend_from_slice(b.as_ref()),
                Err(e) => {
                    yield Err(e);
                    return;
                }
            }
            while let Some(pos) = carry.iter().position(|&c| c == b'\n') {
                let line: Vec<u8> = carry.drain(..=pos).collect();
                if let Some(payload) = parse_line(&line) {
                    yield Ok(payload);
                }
            }
        }
        if let Some(payload) = parse_line(&carry) {
            yield Ok(payload);
        }
    }
}

fn parse_line(line: &[u8]) -> Option<String> {
    let s = String::from_utf8_lossy(line);
    let s = s.trim_end_matches(['\r', '\n']);
    let payload = s.strip_prefix("data:")?.trim();
    (!payload.is_empty()).then(|| payload.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;

    #[tokio::test]
    async fn splits_across_chunk_boundaries() {
        // deliberately split mid-line and mid-payload
        let chunks: Vec<Result<&'static [u8], reqwest::Error>> = vec![
            Ok(b"data: {\"a\""),
            Ok(b":1}\n\ndata: [DO"),
            Ok(b"NE]\n"),
        ];
        let out: Vec<String> = data_payloads(futures::stream::iter(chunks))
            .map(|r| r.unwrap())
            .collect()
            .await;
        assert_eq!(out, vec!["{\"a\":1}", "[DONE]"]);
    }

    #[test]
    fn ignores_non_data_and_empty() {
        assert_eq!(parse_line(b"event: foo"), None);
        assert_eq!(parse_line(b"data:"), None);
        assert_eq!(parse_line(b"data:  hi\r\n"), Some("hi".into()));
    }
}
