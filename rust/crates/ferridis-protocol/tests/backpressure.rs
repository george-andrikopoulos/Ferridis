//! RED tests for Task 7 — BackpressureSignal + StreamChunk (v0.4).

use ferridis_protocol::backpressure::{BackpressureSignal, StreamChunk};
use serde_json::json;

/// StreamChunk carries data and a backpressure signal.
#[test]
fn stream_chunk_carries_data_and_signal() {
    let chunk = StreamChunk::new(json!({"k": "v"}), BackpressureSignal::Continue);
    assert_eq!(chunk.data(), &json!({"k": "v"}));
    assert!(matches!(chunk.signal(), BackpressureSignal::Continue));
}

/// BackpressureSignal::Continue is the default when no signal is given.
#[test]
fn continue_signal_is_the_default() {
    let chunk = StreamChunk::data_only(json!(42));
    assert!(matches!(chunk.signal(), BackpressureSignal::Continue));
}

/// SlowDown signals the consumer to pause before the next read.
#[test]
fn slow_down_variant_exists() {
    let chunk = StreamChunk::new(json!(null), BackpressureSignal::SlowDown);
    assert!(matches!(chunk.signal(), BackpressureSignal::SlowDown));
}

/// Halt signals the consumer to stop reading immediately.
#[test]
fn halt_variant_exists() {
    let chunk = StreamChunk::new(json!(null), BackpressureSignal::Halt);
    assert!(matches!(chunk.signal(), BackpressureSignal::Halt));
}

/// BackpressureSignal derives Debug.
#[test]
fn signal_is_debug() {
    let s = format!("{:?}", BackpressureSignal::Continue);
    assert!(!s.is_empty());
}
