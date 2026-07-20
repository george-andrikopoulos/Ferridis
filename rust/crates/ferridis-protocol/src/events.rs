//! Event channel subscriptions over Server-Sent Events.
//!
//! v0.2 shipped the minimum-viable channel layer: one-way
//! **server → client** push over SSE, with the channel URL discovered
//! by convention (`{base_url}/events/{channel}`). This is enough for
//! notifications, status updates, and event sourcing — the dominant
//! use cases in the early adapter zoo.
//!
//! v0.3 added a sibling [`ws`](crate::ws) module for fully
//! bidirectional channels (subscribe / unsubscribe / ack / control
//! messages travelling upstream alongside `ServerEvent`s coming down).
//! A capability's manifest picks per-channel which transport to use;
//! both primitives surface the same [`ServerEvent`] shape downstream
//! so consumer code can be transport-agnostic.
//!
//! Auto-reconnect with `Last-Event-ID` cursor remains queued.
//!
//! # Wire format
//!
//! Standard SSE per [WHATWG / W3C](https://html.spec.whatwg.org/multipage/server-sent-events.html).
//! Each event block is a sequence of `field: value` lines terminated
//! by a blank line:
//!
//! ```text
//! event: new-message
//! id: 42
//! data: {"sender":"alice","body":"hi"}
//!
//! event: ping
//! data:
//!
//! ```
//!
//! Multi-line `data:` is joined with `\n`. Missing `event:` defaults
//! to `"message"`. `data:` is required; events without it are
//! dropped (matching the SSE spec). Comments (`: comment`) are
//! ignored. The `retry:` field is parsed but ignored — caller-side
//! reconnect/backoff is a v0.3 concern.
//!
//! # Lifetime
//!
//! The returned stream lives until the server closes the connection
//! or the caller drops the stream. Network errors mid-stream surface
//! as a final `Err(ProtocolError)` item before the stream ends.

use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::client::Client;
use crate::error::ProtocolError;

/// The last event ID seen by the client, used as the `Last-Event-ID`
/// header on SSE reconnect to resume the stream from where it left off.
///
/// Constructed only via [`ReconnectCursor::new`]; the inner string is
/// guaranteed non-empty and non-whitespace-only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconnectCursor(String);

impl ReconnectCursor {
    /// Construct a cursor from the raw event ID string.
    ///
    /// Returns `Err` when `id` is empty or whitespace-only.
    pub fn new(id: impl Into<String>) -> Result<Self, ProtocolError> {
        let id = id.into();
        if id.trim().is_empty() {
            return Err(ProtocolError::Json(
                "ReconnectCursor: event ID must not be empty or whitespace-only".to_string(),
            ));
        }
        Ok(Self(id))
    }

    /// The raw event ID value, suitable for use as a `Last-Event-ID` header value.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ReconnectCursor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// One server-sent event, after parsing.
///
/// `data` is parsed as JSON when possible (the overwhelmingly common
/// shape for Ferridis events); when the data is not valid JSON it is
/// surfaced as a JSON string so callers always see a `Value`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerEvent {
    /// The event name. SSE's `event:` field. Defaults to `"message"`
    /// per the spec when the field is absent.
    pub name: String,
    /// The event payload. SSE's `data:` field. Multi-line `data:`
    /// lines are joined with `\n` before this parse.
    pub data: serde_json::Value,
    /// The event id, if the server sent one. SSE's `id:` field.
    /// Servers use this for resume-from-cursor on reconnect; the
    /// v0.2 client just surfaces it.
    pub id: Option<String>,
}

/// Open an SSE connection to `url` and return a stream of parsed
/// events.
///
/// The stream yields `Ok(ServerEvent)` for each well-formed event
/// block, and `Err(ProtocolError)` for transport / parse errors.
/// After an error item the stream may still produce more events if
/// the server is healthy and the error was transient (e.g., a
/// malformed `data:` line); however a transport-level error
/// terminates the stream.
///
/// `Accept: text/event-stream` is set automatically. The server is
/// expected to reply with `200 OK` and `Content-Type: text/event-stream`.
pub async fn subscribe(
    client: &Client,
    url: &Url,
) -> Result<impl Stream<Item = Result<ServerEvent, ProtocolError>> + use<>, ProtocolError> {
    let response = client
        .http()
        .get(url.clone())
        .header(reqwest::header::ACCEPT, "text/event-stream")
        .send()
        .await
        .map_err(|source| ProtocolError::Transport {
            url: url.clone(),
            source,
        })?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        let body = truncate(&body, 512);
        return Err(ProtocolError::BadStatus {
            url: url.clone(),
            status: status.as_u16(),
            body,
        });
    }

    let stream_url = url.clone();
    // `reqwest::Response::bytes_stream` is not `Unpin`; box-pin it so
    // the SSE state machine can poll it without juggling pin
    // projections.
    let bytes = Box::pin(response.bytes_stream().map(move |chunk| {
        chunk.map_err(|source| ProtocolError::Transport {
            url: stream_url.clone(),
            source,
        })
    }));
    Ok(parse_sse_stream(bytes))
}

/// Like [`subscribe`] but sends `Last-Event-ID: {cursor}` so the server
/// can resume the stream from after the last event the client saw.
///
/// Pass `None` for the initial connection (no prior cursor). Pass
/// `Some(&cursor)` on reconnect, where `cursor` is built from the
/// `id` field of the last successfully-received [`ServerEvent`].
pub async fn subscribe_with_cursor(
    client: &Client,
    url: &Url,
    cursor: Option<&ReconnectCursor>,
) -> Result<impl Stream<Item = Result<ServerEvent, ProtocolError>> + use<>, ProtocolError> {
    let mut req = client
        .http()
        .get(url.clone()) // clone: url is borrowed, reqwest::RequestBuilder takes ownership
        .header(reqwest::header::ACCEPT, "text/event-stream");

    if let Some(c) = cursor {
        req = req.header("Last-Event-ID", c.as_str());
    }

    let response = req
        .send()
        .await
        .map_err(|source| ProtocolError::Transport {
            url: url.clone(), // clone: url is borrowed, ProtocolError needs owned Url
            source,
        })?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        let body = truncate(&body, 512);
        return Err(ProtocolError::BadStatus {
            url: url.clone(), // clone: url is borrowed, ProtocolError needs owned Url
            status: status.as_u16(),
            body,
        });
    }

    let stream_url = url.clone(); // clone: url is borrowed from caller; stream closure must own it
    let bytes = Box::pin(response.bytes_stream().map(move |chunk| {
        chunk.map_err(|source| ProtocolError::Transport {
            url: stream_url.clone(), // clone: stream_url is captured and used per-chunk
            source,
        })
    }));
    Ok(parse_sse_stream(bytes))
}

/// Turn a stream of byte chunks into a stream of parsed
/// [`ServerEvent`]s. Pure function — testable without HTTP. Exposed
/// for adapter authors who want to apply the SSE parser to their own
/// byte sources.
pub fn parse_sse_stream<S>(bytes: S) -> impl Stream<Item = Result<ServerEvent, ProtocolError>>
where
    S: Stream<Item = Result<Bytes, ProtocolError>> + Unpin,
{
    async_stream::stream_from_bytes(bytes)
}

// Hand-rolled state-machine wrapper around the byte stream. Buffers
// partial lines across chunks and emits one `ServerEvent` per
// terminator (blank line).
mod async_stream {
    use super::*;
    use std::pin::Pin;
    use std::task::{Context, Poll};

    use futures_util::Stream;

    pub(super) fn stream_from_bytes<S>(
        inner: S,
    ) -> impl Stream<Item = Result<ServerEvent, ProtocolError>>
    where
        S: Stream<Item = Result<Bytes, ProtocolError>> + Unpin,
    {
        SseStream {
            inner,
            buffer: Vec::new(),
            block: EventBlock::default(),
        }
    }

    #[derive(Default)]
    struct EventBlock {
        name: Option<String>,
        data_lines: Vec<String>,
        id: Option<String>,
    }

    impl EventBlock {
        fn is_empty(&self) -> bool {
            self.name.is_none() && self.data_lines.is_empty() && self.id.is_none()
        }

        fn into_event(self) -> Option<Result<ServerEvent, ProtocolError>> {
            if self.data_lines.is_empty() {
                // SSE spec: events without a `data:` field are dropped.
                return None;
            }
            let raw = self.data_lines.join("\n");
            // Parse as JSON if we can; otherwise surface as a JSON
            // string so callers always see a `Value`. Written as
            // `match` (not `unwrap_or_else`) to keep the `raw`
            // ownership story unambiguous and clippy happy.
            let data = match serde_json::from_str::<serde_json::Value>(&raw) {
                Ok(v) => v,
                Err(_) => serde_json::Value::String(raw),
            };
            Some(Ok(ServerEvent {
                name: self.name.unwrap_or_else(|| "message".to_string()),
                data,
                id: self.id,
            }))
        }
    }

    fn apply_line(block: &mut EventBlock, line: &str) {
        // Comment.
        if line.starts_with(':') {
            return;
        }
        let (field, value) = match line.split_once(':') {
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            None => (line, ""),
        };
        match field {
            "event" => block.name = Some(value.to_string()),
            "data" => block.data_lines.push(value.to_string()),
            "id" => block.id = Some(value.to_string()),
            // `retry:` and any unknown field are silently ignored per
            // the spec's "ignore unknowns" guidance.
            _ => {}
        }
    }

    struct SseStream<S> {
        inner: S,
        buffer: Vec<u8>,
        block: EventBlock,
    }

    impl<S> Stream for SseStream<S>
    where
        S: Stream<Item = Result<Bytes, ProtocolError>> + Unpin,
    {
        type Item = Result<ServerEvent, ProtocolError>;

        fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            // Safety: only field accesses, no self-referential moves.
            let this = self.get_mut();

            loop {
                // Try to emit any complete event already in the buffer.
                if let Some(emit) = drain_one(&mut this.buffer, &mut this.block) {
                    return Poll::Ready(Some(emit));
                }

                // Pull more bytes from the upstream.
                match Pin::new(&mut this.inner).poll_next(cx) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(None) => {
                        // Upstream closed. Flush whatever's left.
                        if let Some(emit) = drain_one(&mut this.buffer, &mut this.block) {
                            return Poll::Ready(Some(emit));
                        }
                        return Poll::Ready(None);
                    }
                    Poll::Ready(Some(Err(e))) => return Poll::Ready(Some(Err(e))),
                    Poll::Ready(Some(Ok(chunk))) => {
                        this.buffer.extend_from_slice(&chunk);
                        // Loop back and try to drain another event.
                    }
                }
            }
        }
    }

    /// Try to emit one complete event from `buffer`. Consumes the
    /// bytes for that event from the front of the buffer. Returns
    /// `None` if no terminator is present yet.
    fn drain_one(
        buffer: &mut Vec<u8>,
        block: &mut EventBlock,
    ) -> Option<Result<ServerEvent, ProtocolError>> {
        loop {
            // Find the next line boundary (\n).
            let nl = buffer.iter().position(|&b| b == b'\n')?;
            // Take the line (without the trailing \n).
            let mut line: Vec<u8> = buffer.drain(..=nl).collect();
            line.pop(); // drop \n
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            if line.is_empty() {
                // Terminator: emit the current block if it has anything.
                if !block.is_empty() {
                    let finished = std::mem::take(block);
                    if let Some(emit) = finished.into_event() {
                        return Some(emit);
                    }
                }
                // Else: keep looking for the next non-empty line.
                continue;
            }
            // Line content — append to the in-flight block.
            let text = match std::str::from_utf8(&line) {
                Ok(s) => s,
                Err(_) => {
                    // Non-UTF-8 SSE line. Spec says to ignore lines we
                    // cannot decode; we surface as a parse error so
                    // operators see something is wrong with the
                    // upstream rather than silently dropping data.
                    return Some(Err(ProtocolError::Json(format!(
                        "non-UTF-8 SSE line: {} bytes",
                        line.len()
                    ))));
                }
            };
            apply_line(block, text);
        }
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…(truncated)", &s[..max])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use futures_util::{StreamExt, stream};

    fn bytes_stream(chunks: Vec<&'static str>) -> impl Stream<Item = Result<Bytes, ProtocolError>> {
        stream::iter(
            chunks
                .into_iter()
                .map(|s| Ok(Bytes::copy_from_slice(s.as_bytes()))),
        )
    }

    #[tokio::test]
    async fn parses_canonical_event_with_json_data() {
        let s = bytes_stream(vec![
            "event: new-message\nid: 42\ndata: {\"sender\":\"alice\"}\n\n",
        ]);
        let mut out = Box::pin(parse_sse_stream(s));
        let ev = out.next().await.unwrap().unwrap();
        assert_eq!(ev.name, "new-message");
        assert_eq!(ev.id.as_deref(), Some("42"));
        assert_eq!(ev.data["sender"], "alice");
        assert!(out.next().await.is_none());
    }

    #[tokio::test]
    async fn defaults_event_name_to_message_when_absent() {
        let s = bytes_stream(vec!["data: hello\n\n"]);
        let mut out = Box::pin(parse_sse_stream(s));
        let ev = out.next().await.unwrap().unwrap();
        assert_eq!(ev.name, "message");
        // Non-JSON data surfaces as a JSON string so callers always see Value.
        assert_eq!(ev.data, serde_json::Value::String("hello".to_string()));
    }

    #[tokio::test]
    async fn joins_multiline_data_with_newlines() {
        let s = bytes_stream(vec!["data: line1\ndata: line2\ndata: line3\n\n"]);
        let mut out = Box::pin(parse_sse_stream(s));
        let ev = out.next().await.unwrap().unwrap();
        assert_eq!(
            ev.data,
            serde_json::Value::String("line1\nline2\nline3".to_string())
        );
    }

    #[tokio::test]
    async fn drops_event_blocks_with_no_data_field() {
        // Per the SSE spec.
        let s = bytes_stream(vec!["event: heartbeat\nid: 1\n\n", "data: real\n\n"]);
        let mut out = Box::pin(parse_sse_stream(s));
        let ev = out.next().await.unwrap().unwrap();
        assert_eq!(ev.data, serde_json::Value::String("real".to_string()));
    }

    #[tokio::test]
    async fn ignores_comment_lines_starting_with_colon() {
        let s = bytes_stream(vec![": keepalive\ndata: ok\n\n"]);
        let mut out = Box::pin(parse_sse_stream(s));
        let ev = out.next().await.unwrap().unwrap();
        assert_eq!(ev.data, serde_json::Value::String("ok".to_string()));
    }

    #[tokio::test]
    async fn handles_chunk_boundaries_mid_event() {
        // The byte stream splits the event across chunks; the parser
        // must buffer correctly.
        let s = bytes_stream(vec!["event: thi", "ng\ndata: ", "{\"k\":\"", "v\"}\n\n"]);
        let mut out = Box::pin(parse_sse_stream(s));
        let ev = out.next().await.unwrap().unwrap();
        assert_eq!(ev.name, "thing");
        assert_eq!(ev.data["k"], "v");
    }

    #[tokio::test]
    async fn carriage_returns_are_tolerated() {
        // Some servers send \r\n line endings.
        let s = bytes_stream(vec!["event: r\r\ndata: 1\r\n\r\n"]);
        let mut out = Box::pin(parse_sse_stream(s));
        let ev = out.next().await.unwrap().unwrap();
        assert_eq!(ev.name, "r");
    }
}
