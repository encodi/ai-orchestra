//! A tiny scripted HTTP/1.1 server on 127.0.0.1 for tests. Test code only (included with
//! `#[cfg(test)]` in the library and by path from integration tests); never part of the product.
#![allow(dead_code)]

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[derive(Clone)]
pub enum Route {
    Html(String),
    Text(String),
    /// Any status with a body and a content type.
    Raw {
        status: u16,
        content_type: String,
        body: Vec<u8>,
        extra_headers: Vec<(String, String)>,
    },
    Redirect(u16, String),
    /// Answers after a delay.
    Slow(Duration, Box<Route>),
    /// A body of this many bytes of `a`, claiming `text/plain`.
    Huge(usize),
}

pub struct TestServer {
    pub addr: SocketAddr,
    hits: Arc<Mutex<HashMap<String, usize>>>,
    total: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        301 => "Moved Permanently",
        302 => "Found",
        307 => "Temporary Redirect",
        308 => "Permanent Redirect",
        403 => "Forbidden",
        404 => "Not Found",
        410 => "Gone",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "Status",
    }
}

impl TestServer {
    pub async fn start(routes: Vec<(&str, Route)>) -> Self {
        let table: Arc<HashMap<String, Route>> = Arc::new(
            routes
                .into_iter()
                .map(|(p, r)| (p.to_string(), r))
                .collect(),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let hits = Arc::new(Mutex::new(HashMap::new()));
        let total = Arc::new(AtomicUsize::new(0));
        let (h, t) = (hits.clone(), total.clone());
        let task = tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                let (table, hits, total) = (table.clone(), h.clone(), t.clone());
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 2048];
                    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                        match sock.read(&mut chunk).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => buf.extend_from_slice(&chunk[..n]),
                        }
                        if buf.len() > 16 * 1024 {
                            return;
                        }
                    }
                    let head = String::from_utf8_lossy(&buf).into_owned();
                    let path = head
                        .lines()
                        .next()
                        .and_then(|l| l.split(' ').nth(1))
                        .unwrap_or("/")
                        .to_string();
                    let key = path.split('?').next().unwrap_or("/").to_string();
                    *hits.lock().unwrap().entry(key.clone()).or_insert(0) += 1;
                    total.fetch_add(1, Ordering::SeqCst);
                    let mut route = table.get(&key).cloned().unwrap_or(Route::Raw {
                        status: 404,
                        content_type: "text/plain".into(),
                        body: b"not found".to_vec(),
                        extra_headers: vec![],
                    });
                    if let Route::Slow(delay, inner) = route {
                        tokio::time::sleep(delay).await;
                        route = *inner;
                    }
                    let (status, ctype, body, mut headers): (
                        u16,
                        String,
                        Vec<u8>,
                        Vec<(String, String)>,
                    ) = match route {
                        Route::Html(b) => (
                            200,
                            "text/html; charset=utf-8".into(),
                            b.into_bytes(),
                            vec![],
                        ),
                        Route::Text(b) => (
                            200,
                            "text/plain; charset=utf-8".into(),
                            b.into_bytes(),
                            vec![],
                        ),
                        Route::Raw {
                            status,
                            content_type,
                            body,
                            extra_headers,
                        } => (status, content_type, body, extra_headers),
                        Route::Redirect(code, to) => (
                            code,
                            "text/plain".into(),
                            vec![],
                            vec![("Location".into(), to)],
                        ),
                        Route::Huge(n) => (200, "text/plain".into(), vec![b'a'; n], vec![]),
                        Route::Slow(..) => unreachable!("nested slow routes are not supported"),
                    };
                    headers.push(("Content-Type".into(), ctype));
                    headers.push(("Content-Length".into(), body.len().to_string()));
                    headers.push(("Connection".into(), "close".into()));
                    let mut out = format!("HTTP/1.1 {status} {}\r\n", reason(status));
                    for (k, v) in headers {
                        out.push_str(&format!("{k}: {v}\r\n"));
                    }
                    out.push_str("\r\n");
                    let _ = sock.write_all(out.as_bytes()).await;
                    let _ = sock.write_all(&body).await;
                    let _ = sock.shutdown().await;
                });
            }
        });
        Self {
            addr,
            hits,
            total,
            task,
        }
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://{}{}", self.addr, path)
    }

    /// A URL on a host *name* (resolved by the test resolver to 127.0.0.1).
    pub fn named_url(&self, host: &str, path: &str) -> String {
        format!("http://{host}:{}{path}", self.addr.port())
    }

    pub fn hits(&self, path: &str) -> usize {
        self.hits.lock().unwrap().get(path).copied().unwrap_or(0)
    }

    pub fn total_hits(&self) -> usize {
        self.total.load(Ordering::SeqCst)
    }
}
