//! Property-based tests for the SSE wire parser: chunk-boundary
//! invariance (the law behind the hand-written boundary unit tests) and
//! field fidelity for arbitrary well-formed event sequences.

use bytes::Bytes;
use ferridis_protocol::ProtocolError;
use ferridis_protocol::events::{ServerEvent, parse_sse_stream};
use futures_util::{StreamExt as _, stream};
use proptest::prelude::*;

/// One generated SSE event: (name, single-line data, optional id).
fn event() -> impl Strategy<Value = (String, String, Option<String>)> {
    (
        proptest::string::string_regex("[a-z]{1,8}").expect("valid generator regex"),
        proptest::string::string_regex("[a-zA-Z0-9.,_-]{1,24}").expect("valid generator regex"),
        proptest::option::of(
            proptest::string::string_regex("[0-9]{1,6}").expect("valid generator regex"),
        ),
    )
}

fn encode(events: &[(String, String, Option<String>)]) -> String {
    let mut wire = String::new();
    for (name, data, id) in events {
        wire.push_str(&format!("event: {name}\n"));
        wire.push_str(&format!("data: {data}\n"));
        if let Some(id) = id {
            wire.push_str(&format!("id: {id}\n"));
        }
        wire.push('\n');
    }
    wire
}

/// Split `wire` into chunks of the given sizes (greedy; remainder becomes
/// the final chunk). ASCII-only input, so byte cuts are always valid.
fn chunk(wire: &str, sizes: &[usize]) -> Vec<Bytes> {
    let bytes = wire.as_bytes();
    let mut chunks = Vec::new();
    let mut at = 0;
    for &size in sizes {
        if at >= bytes.len() {
            break;
        }
        let end = (at + size).min(bytes.len());
        chunks.push(Bytes::copy_from_slice(&bytes[at..end]));
        at = end;
    }
    if at < bytes.len() {
        chunks.push(Bytes::copy_from_slice(&bytes[at..]));
    }
    chunks
}

fn parse_chunks(chunks: Vec<Bytes>) -> Vec<ServerEvent> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("build test runtime");
    rt.block_on(async {
        let byte_stream = stream::iter(chunks.into_iter().map(Ok::<_, ProtocolError>));
        let mut parsed = Box::pin(parse_sse_stream(byte_stream));
        let mut events = Vec::new();
        while let Some(item) = parsed.next().await {
            events.push(item.expect("well-formed wire input must parse"));
        }
        events
    })
}

proptest! {
    /// The parser's output is invariant under chunk boundaries: parsing
    /// the wire bytes as one chunk and parsing them split at arbitrary
    /// points yields identical event sequences.
    #[test]
    fn parse_is_invariant_under_chunk_boundaries(
        events in proptest::collection::vec(event(), 0..8),
        sizes in proptest::collection::vec(1usize..7, 0..48),
    ) {
        let wire = encode(&events);
        let whole = parse_chunks(vec![Bytes::copy_from_slice(wire.as_bytes())]);
        let split = parse_chunks(chunk(&wire, &sizes));
        prop_assert_eq!(whole, split);
    }

    /// Every generated event comes back with its name and id intact, in
    /// order — no drops, no reorders, no bleed between events.
    #[test]
    fn names_and_ids_survive_the_round_trip(
        events in proptest::collection::vec(event(), 1..8),
    ) {
        let wire = encode(&events);
        let parsed = parse_chunks(vec![Bytes::copy_from_slice(wire.as_bytes())]);
        prop_assert_eq!(parsed.len(), events.len());
        for (got, (name, _data, id)) in parsed.iter().zip(events.iter()) {
            prop_assert_eq!(&got.name, name);
            prop_assert_eq!(&got.id, id);
        }
    }

    /// `\r\n` line endings parse identically to `\n` — the tolerance the
    /// unit tests pin for one case, held across arbitrary sequences.
    #[test]
    fn crlf_and_lf_parse_identically(
        events in proptest::collection::vec(event(), 0..8),
    ) {
        let wire_lf = encode(&events);
        let wire_crlf = wire_lf.replace('\n', "\r\n");
        let from_lf = parse_chunks(vec![Bytes::copy_from_slice(wire_lf.as_bytes())]);
        let from_crlf = parse_chunks(vec![Bytes::copy_from_slice(wire_crlf.as_bytes())]);
        prop_assert_eq!(from_lf, from_crlf);
    }
}
