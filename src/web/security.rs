//! Local security: Host validation, origin and token checks, response headers.
//!
//! The server binds to loopback, but that alone does not protect a local app:
//! * **DNS rebinding** - a hostile page can point its own name at 127.0.0.1. Any request whose `Host`
//!   is not one of ours (`127.0.0.1`, `localhost`, `[::1]` with our port, or a configured extra) is
//!   refused.
//! * **Cross-site requests** - state-changing requests (POST, PUT, DELETE, PATCH) need the per-run
//!   token in `X-AIO-Token` (only the page this server serves knows it; other origins cannot read it),
//!   JSON content, and, when the browser sends an `Origin`, an origin that is ours. There is no CORS: a
//!   cross-origin page cannot read any response.
//! * **Fetch metadata** - browsers label every request with `Sec-Fetch-Site`. Anything not from this origin
//!   (`cross-site`, or `same-site`, which includes another web app on `localhost` with a different port) is
//!   refused, except a plain top-level navigation by GET, which only shows the page. This is defence in depth
//!   on top of the Origin and token checks, and it also covers reads, which carry no token.
//! * **Content** - a strict Content-Security-Policy (no remote scripts, fonts or styles, no framing),
//!   `nosniff`, no referrer, no caching of API answers.

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use super::AppState;
use super::error::ApiError;

pub const TOKEN_HEADER: &str = "x-aio-token";

/// A random token for this run of the server (from the operating system's generator).
pub fn new_token() -> String {
    let mut bytes = [0u8; 32];
    #[cfg(unix)]
    {
        use std::io::Read as _;
        if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
            let _ = f.read_exact(&mut bytes);
        }
    }
    if bytes.iter().all(|b| *b == 0) {
        // No system generator: mix what is available. Never a constant.
        use std::hash::{BuildHasher, Hasher};
        for chunk in bytes.chunks_mut(8) {
            let mut h = std::collections::hash_map::RandomState::new().build_hasher();
            h.write_u128(crate::history::now_ms().into());
            h.write_u32(std::process::id());
            chunk.copy_from_slice(&h.finish().to_le_bytes());
        }
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Compares in time that does not depend on where the strings differ.
pub fn constant_time_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.bytes()
        .zip(b.bytes())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

pub fn host_allowed(host: &str, state: &AppState) -> bool {
    let h = host.trim().to_ascii_lowercase();
    let port = state.port;
    [
        format!("127.0.0.1:{port}"),
        format!("localhost:{port}"),
        format!("[::1]:{port}"),
    ]
    .contains(&h)
        || state
            .extra_hosts
            .iter()
            .any(|x| x.to_ascii_lowercase() == h)
}

fn origin_allowed(origin: &str, state: &AppState) -> bool {
    let o = origin.trim().to_ascii_lowercase();
    let Some(host) = o.strip_prefix("http://") else {
        return false;
    };
    host_allowed(host, state)
}

fn is_state_changing(m: &Method) -> bool {
    matches!(
        *m,
        Method::POST | Method::PUT | Method::DELETE | Method::PATCH
    )
}

fn header_str<'a>(h: &'a HeaderMap, name: &str) -> Option<&'a str> {
    h.get(name).and_then(|v| v.to_str().ok())
}

pub async fn guard(State(state): State<Arc<AppState>>, req: Request, next: Next) -> Response {
    let headers = req.headers();
    match header_str(headers, "host") {
        Some(h) if host_allowed(h, &state) => {}
        _ => {
            return ApiError::new(
                StatusCode::MISDIRECTED_REQUEST,
                "forbidden",
                "this server only answers requests addressed to its own local address",
            )
            .into_response();
        }
    }
    if let Some(origin) = header_str(headers, "origin")
        && !origin_allowed(origin, &state)
    {
        return ApiError::forbidden("cross-origin requests are not accepted").into_response();
    }
    if let Some(site) = header_str(headers, "sec-fetch-site") {
        let ours = matches!(site, "same-origin" | "none");
        let navigation = req.method() == Method::GET
            && header_str(headers, "sec-fetch-mode") == Some("navigate");
        if !ours && !navigation {
            return ApiError::forbidden("requests from other sites are not accepted")
                .into_response();
        }
    }
    if req.method() == Method::OPTIONS {
        return ApiError::new(
            StatusCode::METHOD_NOT_ALLOWED,
            "forbidden",
            "there is no CORS: only the page served by this server may call the API",
        )
        .into_response();
    }
    if is_state_changing(req.method()) {
        let ok =
            header_str(headers, TOKEN_HEADER).is_some_and(|t| constant_time_eq(t, &state.token));
        if !ok {
            return ApiError::forbidden("missing or wrong session token (reload the page)")
                .into_response();
        }
        let json = header_str(headers, "content-type")
            .is_some_and(|c| c.to_ascii_lowercase().starts_with("application/json"));
        let has_body = headers
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|n| n != "0")
            || headers.contains_key("transfer-encoding");
        if has_body && !json {
            return ApiError::new(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "validation",
                "request bodies must be application/json",
            )
            .into_response();
        }
    }
    let is_api = req.uri().path().starts_with("/api/");
    let mut res = next.run(req).await;
    let h = res.headers_mut();
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; font-src 'self'; connect-src 'self'; manifest-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'"),
    );
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    h.insert("x-frame-options", HeaderValue::from_static("DENY"));
    h.insert(
        "cross-origin-resource-policy",
        HeaderValue::from_static("same-origin"),
    );
    h.insert(
        "cross-origin-opener-policy",
        HeaderValue::from_static("same-origin"),
    );
    h.insert(
        "permissions-policy",
        HeaderValue::from_static("camera=(), microphone=(), geolocation=()"),
    );
    if is_api {
        h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_long_random_and_compared_exactly() {
        let (a, b) = (new_token(), new_token());
        assert_eq!(a.len(), 64);
        assert_ne!(a, b);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(constant_time_eq(&a, &a.clone()));
        assert!(!constant_time_eq(&a, &b));
        assert!(!constant_time_eq("abc", "abcd"));
    }
}
