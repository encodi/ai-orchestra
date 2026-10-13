//! The compiled web interface, embedded in the binary by `build.rs` from `web/dist`.
//!
//! Only files that exist in that table are served (a request path is looked up, never joined onto a
//! directory), so there is no path to traverse and no way to reach the database or any other file.

include!(concat!(env!("OUT_DIR"), "/web_assets.rs"));

pub const TOKEN_PLACEHOLDER: &str = "__AIO_TOKEN__";

pub fn mime_for(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "json" | "webmanifest" => "application/json",
        "woff2" => "font/woff2",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

pub fn get(path: &str) -> Option<&'static [u8]> {
    EMBEDDED
        .iter()
        .find(|(name, _)| *name == path)
        .map(|(_, bytes)| *bytes)
}

pub fn has_frontend() -> bool {
    get("index.html").is_some()
}

pub const NOT_BUILT_PAGE: &str = "<!doctype html><html lang=\"en\"><meta charset=\"utf-8\"><title>AI Orchestra</title>\
<body style=\"font-family:system-ui;max-width:42rem;margin:4rem auto;padding:0 1rem\"><h1>AI Orchestra</h1>\
<p>The local server is running, but this build does not include the web interface.</p>\
<p>Build it once with <code>cd web &amp;&amp; npm install &amp;&amp; npm run build</code>, then rebuild with \
<code>cargo build --release</code>. The API is available at <code>/api/v1/health</code>.</p></body></html>";
