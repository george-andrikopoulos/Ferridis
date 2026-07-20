//! Backpressure / flow-control types for streamed intents.
//!
//! Wire shape (v0.7): the adapter's SSE stream carries an
//! `event: backpressure` with `data: {"signal": "slow-down" | "halt"}`
//! whenever the signal *changes* from the previous state. `continue`
//! is the implicit initial state and is never sent. `halt` terminates
//! the stream — no further chunks and no `end` event follow it, and
//! consumers surface it as a typed error rather than a clean finish.
//! Clients that predate this event ignore it per SSE convention.

use serde::{Deserialize, Serialize};

/// A flow-control signal embedded in each chunk of a streamed intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BackpressureSignal {
    /// Normal pace — the consumer should continue reading.
    #[default]
    Continue,
    /// The adapter's queue is filling; the consumer should pause briefly.
    SlowDown,
    /// The adapter is overwhelmed; the consumer must stop reading.
    Halt,
}

/// One chunk in a streamed intent response, paired with a
/// [`BackpressureSignal`] from the adapter.
#[derive(Debug, Clone)]
pub struct StreamChunk {
    data: serde_json::Value,
    signal: BackpressureSignal,
}

impl StreamChunk {
    /// Construct a chunk with an explicit backpressure signal.
    pub fn new(data: serde_json::Value, signal: BackpressureSignal) -> Self {
        Self { data, signal }
    }

    /// Construct a chunk with [`BackpressureSignal::Continue`] — the common case.
    pub fn data_only(data: serde_json::Value) -> Self {
        Self {
            data,
            signal: BackpressureSignal::Continue,
        }
    }

    /// The chunk payload.
    pub fn data(&self) -> &serde_json::Value {
        &self.data
    }

    /// The backpressure signal from the adapter.
    pub fn signal(&self) -> BackpressureSignal {
        self.signal
    }
}
