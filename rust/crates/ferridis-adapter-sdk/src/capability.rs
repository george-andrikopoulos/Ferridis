//! The [`Capability`] trait — what an adapter implements.

use std::pin::Pin;

use async_trait::async_trait;
use ferridis_core::{IntentVerb, Manifest};
use futures_util::Stream;

use ferridis_protocol::StreamChunk;

use crate::dispatch::DispatchError;

/// One item from a streamed dispatch — either a successful chunk or
/// a typed dispatch failure that terminates the stream early.
pub type StreamItem = Result<serde_json::Value, DispatchError>;

/// A boxed, pinned, `Send` stream of [`StreamItem`]s. Returned by
/// [`Capability::dispatch_stream`]. Boxed because async-trait method
/// return types can't be `impl Trait` directly without breaking
/// object safety.
pub type IntentStream = Pin<Box<dyn Stream<Item = StreamItem> + Send>>;

/// One item from a flow-aware streamed dispatch: a [`StreamChunk`]
/// (payload plus [`ferridis_protocol::BackpressureSignal`]) or a typed
/// dispatch failure that terminates the stream early.
pub type FlowStreamItem = Result<StreamChunk, DispatchError>;

/// A boxed, pinned, `Send` stream of [`FlowStreamItem`]s. Returned by
/// [`Capability::dispatch_stream_flow`].
pub type FlowIntentStream = Pin<Box<dyn Stream<Item = FlowStreamItem> + Send>>;

/// How the SDK should source the OpenAPI / AsyncAPI schema document.
///
/// The schema is loaded lazily by callers; the SDK only needs to know
/// how to produce its bytes when asked.
#[derive(Debug, Clone)]
pub enum SchemaSource {
    /// Schema is embedded as a string. The SDK serves it directly.
    Embedded {
        /// MIME type to advertise (e.g. `application/yaml`,
        /// `application/json`).
        content_type: String,
        /// The raw schema text.
        body: String,
    },
    /// Schema lives at another URL — the SDK responds with a redirect
    /// instead of serving the bytes itself.
    Redirect {
        /// Target URL.
        url: url::Url,
    },
}

/// What an adapter implements to publish a Ferridis capability.
///
/// Implementations must be `Send + Sync + 'static` because the SDK
/// shares them across multiple request handlers.
///
/// # ⚠ Input validation in v0.1
///
/// **The v0.1 SDK does not validate request bodies against the
/// capability's declared OpenAPI / AsyncAPI schema before dispatch.**
/// The `body` passed to [`Capability::dispatch`] is the raw, parsed JSON
/// the client sent — its shape has been syntactically checked (valid
/// JSON) but not semantically (matches the declared schema).
///
/// **Implementers are therefore responsible for input validation.** A
/// typical pattern is:
///
/// 1. Deserialize the body into a strict, typed argument struct with
///    [`serde::Deserialize`]; treat deserialization failure as
///    [`DispatchError::InvalidRequest`].
/// 2. Apply any additional invariants the type system can't enforce
///    (path traversal, length bounds, allow-listed values).
///
/// A less defensive adapter — one that simply trusts `body` — exposes
/// itself to malformed-input attacks. The reference filesystem adapter
/// (`ferridis-adapter-fs`) shows the pattern: every intent runs its
/// user-supplied paths through the `Root` / `RelPath` typestate before
/// touching the filesystem.
///
/// **Schema-driven validation lands in v0.2** once an OpenAPI runtime
/// validator is chosen for the workspace. At that point the SDK will
/// reject malformed requests with `400` before they reach
/// [`dispatch`](Self::dispatch).
#[async_trait]
pub trait Capability: Send + Sync + 'static {
    /// The manifest this capability publishes. The SDK serves it from
    /// the `GET /manifest.json` route.
    fn manifest(&self) -> &Manifest;

    /// Where the schema lives.
    fn schema(&self) -> SchemaSource;

    /// JSON Schema for the request body of `intent`, if the adapter wants
    /// the SDK to validate incoming bodies before dispatch.
    ///
    /// Return `Some(schema)` to opt in to v0.4 schema-driven validation.
    /// The SDK rejects non-conforming requests with `422 Unprocessable
    /// Entity` before they reach [`dispatch`](Self::dispatch).
    ///
    /// Default returns `None` so existing adapters keep compiling unchanged.
    fn body_schema(&self, _intent: &IntentVerb) -> Option<serde_json::Value> {
        None
    }

    /// Dispatch a single-shot (request-kind) intent call.
    ///
    /// `body` is the parsed JSON request payload. See the
    /// [crate-level note on input validation](Capability#input-validation-in-v01)
    /// — in v0.1 the implementer owns body validation.
    ///
    /// For stream-kind intents (those the manifest declares as
    /// `kind: "stream"`), implement [`Self::dispatch_stream`] instead;
    /// the SDK routes by manifest-declared kind, so a request-kind
    /// intent will never reach this method with a streaming caller.
    async fn dispatch(
        &self,
        intent: &IntentVerb,
        body: serde_json::Value,
    ) -> Result<serde_json::Value, DispatchError>;

    /// Dispatch a stream-kind intent and return an ordered stream of
    /// chunks. The SDK wraps the stream in an SSE response: each
    /// `Ok(value)` becomes an `event: chunk` line, each `Err(e)`
    /// terminates with an `event: error`, and stream-exhausted
    /// produces a final `event: end`.
    ///
    /// Default impl returns [`DispatchError::UnsupportedIntent`] so
    /// existing adapters that don't declare any stream-kind intents
    /// keep compiling without change. Implementers of capabilities
    /// with stream-kind intents in the manifest must override.
    async fn dispatch_stream(
        &self,
        intent: &IntentVerb,
        _body: serde_json::Value,
    ) -> Result<IntentStream, DispatchError> {
        Err(DispatchError::UnsupportedIntent(intent.clone())) // clone: error carries an owned verb
    }

    /// Flow-aware variant of [`Self::dispatch_stream`]: each chunk
    /// carries a [`ferridis_protocol::BackpressureSignal`] alongside
    /// its payload. The SDK routes stream-kind intents through *this*
    /// method and emits an `event: backpressure` on the SSE wire
    /// whenever the signal changes; a `Halt` terminates the stream.
    ///
    /// The default impl adapts [`Self::dispatch_stream`], tagging every
    /// chunk `Continue` — existing adapters keep their exact behavior
    /// without change. Override only when the adapter can meaningfully
    /// signal `SlowDown` / `Halt` (e.g., a bounded internal queue).
    async fn dispatch_stream_flow(
        &self,
        intent: &IntentVerb,
        body: serde_json::Value,
    ) -> Result<FlowIntentStream, DispatchError> {
        let inner = self.dispatch_stream(intent, body).await?;
        Ok(Box::pin(futures_util::StreamExt::map(inner, |item| {
            item.map(StreamChunk::data_only)
        })))
    }
}
