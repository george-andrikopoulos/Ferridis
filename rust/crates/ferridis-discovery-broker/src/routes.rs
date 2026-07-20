//! Axum route handlers for the Ferridis discovery broker.

use crate::store::{BrokerError, RegisterRequest, ServiceStore};
use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
    routing::{delete, get, post},
};
use ferridis_protocol::discovery::ServiceKind;
use futures_core::Stream;
use serde_json::json;
use tokio_stream::{StreamExt as _, wrappers::BroadcastStream};

/// Build the axum router with all discovery routes.
pub fn make_router(store: ServiceStore) -> Router {
    Router::new()
        .route("/discovery/register", post(register))
        .route("/discovery/services", get(list_services))
        .route("/discovery/services/:name", delete(delete_service))
        .route("/discovery/events", get(events))
        .with_state(store)
}

async fn register(State(store): State<ServiceStore>, Json(req): Json<RegisterRequest>) -> Response {
    match store.register(req).await {
        Ok(()) => (StatusCode::OK, Json(json!({"ok": true}))).into_response(),
        Err(BrokerError::UnknownKind(k)) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({"error": format!("unknown kind: {k}")})),
        )
            .into_response(),
        Err(BrokerError::InvalidUrl(e)) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({"error": format!("invalid url: {e}")})),
        )
            .into_response(),
        Err(BrokerError::Io(e)) => {
            tracing::error!(error = %e, "state file I/O error during register");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "state persistence failed"})),
            )
                .into_response()
        }
        Err(BrokerError::Json(e)) => {
            tracing::error!(error = %e, "state file JSON error during register");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "state persistence failed"})),
            )
                .into_response()
        }
    }
}

async fn list_services(State(store): State<ServiceStore>) -> Response {
    let services: Vec<serde_json::Value> = store
        .list()
        .await
        .into_iter()
        .map(|e| {
            json!({
                "name": e.service().name(),
                "kind": kind_str(e.service().kind()),
                "url": e.service().url().as_str(),
                "persistent": e.persistent(),
            })
        })
        .collect();
    Json(services).into_response()
}

async fn delete_service(State(store): State<ServiceStore>, Path(name): Path<String>) -> Response {
    match store.remove(&name).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            tracing::error!(error = %e, name = %name, "state file error during delete");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "state persistence failed"})),
            )
                .into_response()
        }
    }
}

async fn events(
    State(store): State<ServiceStore>,
) -> Sse<impl Stream<Item = Result<Event, axum::Error>>> {
    let rx = store.subscribe();
    let stream = BroadcastStream::new(rx).filter_map(|result| {
        result.ok().map(|svc| {
            let data = json!({
                "name": svc.name(),
                "kind": kind_str(svc.kind()),
                "url": svc.url().as_str(),
            });
            Ok(Event::default().data(data.to_string()))
        })
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

fn kind_str(kind: ServiceKind) -> &'static str {
    match kind {
        ServiceKind::Mcp => "mcp",
        ServiceKind::Ferridis => "ferridis",
    }
}
