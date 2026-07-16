//! Shared request/response helpers for the v1 and v2 handlers.

use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use cadvisor_model::GoTime;

pub fn plain(status: StatusCode, body: String) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        format!("{body}\n"),
    )
        .into_response()
}

pub fn json<T: serde::Serialize>(value: &T) -> Response {
    match serde_json::to_vec(value) {
        Ok(body) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json")],
            body,
        )
            .into_response(),
        Err(e) => plain(StatusCode::INTERNAL_SERVER_ERROR, format!("failed to marshal: {e}")),
    }
}

/// Lenient query-param access (upstream ignores unparsable values).
pub struct Params(Vec<(String, String)>);

impl Params {
    pub fn parse(raw: &Option<String>) -> Self {
        let mut out = Vec::new();
        if let Some(raw) = raw {
            for pair in raw.split('&') {
                let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
                out.push((url_decode(k), url_decode(v)));
            }
        }
        Params(out)
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.0.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    pub fn get_bool(&self, key: &str) -> bool {
        self.get(key).map(|v| v == "true").unwrap_or(false)
    }

    pub fn get_time(&self, key: &str) -> GoTime {
        self.get(key)
            .and_then(|v| {
                serde_json::from_value::<GoTime>(serde_json::Value::String(v.to_string())).ok()
            })
            .unwrap_or_default()
    }
}

pub fn url_decode(s: &str) -> String {
    let mut out = Vec::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                if let Ok(b) = u8::from_str_radix(hex, 16) {
                    out.push(b);
                    i += 3;
                } else {
                    out.push(bytes[i]);
                    i += 1;
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}
