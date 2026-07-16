//! Route dispatch and v1.0-v1.3 request handling.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{RawQuery, State};
use axum::http::{StatusCode, header};
use axum::response::Response;
use cadvisor_manager::Manager;
use cadvisor_manager::manager::EventQuery;
use cadvisor_model::v1;

use crate::common::{Params, json, plain};

const SUPPORTED_VERSIONS: &str = "v1.0,v1.1,v1.2,v1.3,v2.0,v2.1";

pub fn router(manager: Arc<Manager>) -> axum::Router {
    use axum::routing::any;
    axum::Router::new()
        .route("/api", any(api_root))
        .route("/api/", any(api_root))
        .route("/api/{*rest}", any(dispatch))
        .with_state(manager)
}

async fn api_root() -> Response {
    plain(StatusCode::BAD_REQUEST, format!("Supported API versions: {SUPPORTED_VERSIONS}"))
}

async fn dispatch(
    State(manager): State<Arc<Manager>>,
    axum::extract::Path(rest): axum::extract::Path<String>,
    RawQuery(raw_query): RawQuery,
    body: Bytes,
) -> Response {
    let mut segments = rest.split('/');
    let version = segments.next().unwrap_or_default().to_string();
    let request_type = segments.next().unwrap_or_default().to_string();
    let args: Vec<&str> = segments.collect();
    let container_name = if args.is_empty() || (args.len() == 1 && args[0].is_empty()) {
        "/".to_string()
    } else {
        format!("/{}", args.join("/")).trim_end_matches('/').to_string()
    };
    let container_name = if container_name.is_empty() { "/".to_string() } else { container_name };
    let params = Params::parse(&raw_query);

    match version.as_str() {
        "v1.0" | "v1.1" | "v1.2" | "v1.3" => {
            handle_v1(&manager, &version, &request_type, &container_name, &params, &body).await
        }
        "v2.0" | "v2.1" => {
            if request_type == "events" {
                handle_events(&manager, &container_name, &params).await
            } else if request_type.is_empty() {
                plain(
                    StatusCode::BAD_REQUEST,
                    r#"Supported request types: "attributes","events","machine","machinestats","ps","spec","stats","storage","summary","version","appmetrics""#.to_string(),
                )
            } else {
                crate::v2::handle_v2(&manager, &version, &request_type, &container_name, &params)
                    .await
            }
        }
        _ => plain(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("unsupported API version {version:?}"),
        ),
    }
}

fn decode_request(body: &Bytes) -> v1::ContainerInfoRequest {
    if body.is_empty() {
        return v1::ContainerInfoRequest::default();
    }
    serde_json::from_slice(body).unwrap_or_default()
}

async fn handle_v1(
    manager: &Arc<Manager>,
    version: &str,
    request_type: &str,
    container_name: &str,
    params: &Params,
    body: &Bytes,
) -> Response {
    match (version, request_type) {
        (_, "") => plain(
            StatusCode::BAD_REQUEST,
            r#"Supported request types: "containers","docker","events","machine","subcontainers""#.to_string(),
        ),
        (_, "machine") => json(&manager.machine_info()),
        (_, "containers") => {
            let req = decode_request(body);
            match manager.container_info(container_name, &req) {
                Ok(info) => json(&info),
                Err(e) => plain(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("failed to get container {container_name:?} with error: {e}"),
                ),
            }
        }
        ("v1.1" | "v1.2" | "v1.3", "subcontainers") => {
            let req = decode_request(body);
            match manager.subcontainers_info(container_name, &req) {
                Ok(infos) => json(&infos),
                Err(e) => plain(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("failed to get subcontainers for container {container_name:?} with error: {e}"),
                ),
            }
        }
        ("v1.2" | "v1.3", "docker") => {
            // Answered from runtime (containerd/CRI-O) metadata.
            let id = container_name.trim_start_matches('/');
            let req = decode_request(body);
            match manager.docker_containers(id, &req) {
                Ok(map) => json(&map),
                Err(e) => plain(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("failed to get Docker container {id:?} with error: {e}"),
                ),
            }
        }
        ("v1.3", "events") => handle_events(manager, container_name, params).await,
        (_, other) => plain(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("unknown request type {other:?}"),
        ),
    }
}

fn event_query(container_name: &str, params: &Params) -> EventQuery {
    let all = params.get_bool("all_events");
    let mut event_types = Vec::new();
    if all || params.get_bool("oom_events") {
        event_types.push(v1::EventType::Oom);
    }
    if all || params.get_bool("oom_kill_events") {
        event_types.push(v1::EventType::OomKill);
    }
    if all || params.get_bool("creation_events") {
        event_types.push(v1::EventType::ContainerCreation);
    }
    if all || params.get_bool("deletion_events") {
        event_types.push(v1::EventType::ContainerDeletion);
    }
    EventQuery {
        container_name: container_name.to_string(),
        include_subcontainers: params.get_bool("subcontainers"),
        event_types,
        max_events: params.get("max_events").and_then(|v| v.parse().ok()).unwrap_or(10),
        start_time: params.get_time("start_time"),
        end_time: params.get_time("end_time"),
    }
}

pub(crate) async fn handle_events(
    manager: &Arc<Manager>,
    container_name: &str,
    params: &Params,
) -> Response {
    let q = event_query(container_name, params);
    if !params.get_bool("stream") {
        let events: Vec<v1::Event> = manager.events(&q).iter().map(|e| (**e).clone()).collect();
        return json(&events);
    }
    // Streaming: newline-delimited JSON events, flushed as they occur.
    let mut rx = manager.subscribe_events();
    let (tx, rx_stream) = tokio::sync::mpsc::channel::<Result<Vec<u8>, std::convert::Infallible>>(16);
    tokio::spawn(async move {
        while let Ok(ev) = rx.recv().await {
            if q.matches(&ev) {
                let mut line = match serde_json::to_vec(&*ev) {
                    Ok(l) => l,
                    Err(_) => continue,
                };
                line.push(b'\n');
                if tx.send(Ok(line)).await.is_err() {
                    return;
                }
            }
        }
    });
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .body(axum::body::Body::from_stream(ReceiverStream(rx_stream)))
        .unwrap()
}

/// Minimal Stream impl over an mpsc receiver (avoids a tokio-stream dep).
struct ReceiverStream<T>(tokio::sync::mpsc::Receiver<T>);

impl<T> futures_core::Stream for ReceiverStream<T> {
    type Item = T;
    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<T>> {
        self.0.poll_recv(cx)
    }
}
