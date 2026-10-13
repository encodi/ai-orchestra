//! HTML to readable text, plus the page metadata that can be read without executing anything.
//!
//! Scripts are never run: `<script>` content is dropped from the text, and the only script block
//! that is read at all is `application/ld+json`, which is data. Nothing here is invented: a date
//! or an author that the page does not state stays `None`.

use scraper::{ElementRef, Html, Selector};
use serde_json::Value;
use url::Url;

/// What could be read from a page.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Extracted {
    pub title: Option<String>,
    pub canonical_url: Option<String>,
    pub author: Option<String>,
    /// ISO-like date as the page states it (`2024-05-01` or a full timestamp).
    pub published: Option<String>,
    pub updated: Option<String>,
    pub text: String,
    /// The text was cut at the size limit.
    pub text_truncated: bool,
}

/// Elements whose content is never part of the readable text.
const SKIPPED: &[&str] = &[
    "script", "style", "noscript", "template", "svg", "canvas", "iframe", "object", "embed",
    "head", "form", "select", "button", "nav", "footer", "aside",
];
const BLOCKS: &[&str] = &[
    "p",
    "div",
    "section",
    "article",
    "main",
    "li",
    "ul",
    "ol",
    "tr",
    "table",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "blockquote",
    "pre",
    "br",
    "hr",
    "dt",
    "dd",
    "figcaption",
    "caption",
];

fn sel(css: &str) -> Selector {
    // The selectors below are constants; a typo would be a programming error caught by the tests.
    Selector::parse(css)
        .unwrap_or_else(|_| Selector::parse("never-matches-anything").expect("valid selector"))
}

/// `2024-05-01`, `2024-05-01T10:00:00Z` and similar. Anything else is not trusted as a date.
pub fn normalize_date(raw: &str) -> Option<String> {
    let t = raw.trim();
    // `2017/06/12` (arXiv, some journals) is the same date as `2017-06-12`.
    let slashed;
    let t = if t.len() >= 10
        && t.as_bytes().get(4) == Some(&b'/')
        && t.as_bytes().get(7) == Some(&b'/')
    {
        slashed = format!("{}-{}-{}{}", &t[0..4], &t[5..7], &t[8..10], &t[10..]);
        slashed.as_str()
    } else {
        t
    };
    let b = t.as_bytes();
    let digits =
        |r: std::ops::Range<usize>| b.get(r).is_some_and(|s| s.iter().all(u8::is_ascii_digit));
    let ok = b.len() >= 10
        && digits(0..4)
        && b[4] == b'-'
        && digits(5..7)
        && b[7] == b'-'
        && digits(8..10)
        && (b.len() == 10 || matches!(b[10], b'T' | b' '));
    if !ok {
        return None;
    }
    let (month, day) = (&t[5..7], &t[8..10]);
    let valid = (1..=12).contains(&month.parse::<u32>().ok()?)
        && (1..=31).contains(&day.parse::<u32>().ok()?);
    (valid && t.len() <= 40).then(|| t.to_string())
}

fn meta_content(doc: &Html, selectors: &[&str]) -> Option<String> {
    for css in selectors {
        if let Some(el) = doc.select(&sel(css)).next() {
            let v = el
                .value()
                .attr("content")
                .or_else(|| el.value().attr("datetime"));
            if let Some(v) = v.map(str::trim).filter(|v| !v.is_empty()) {
                return Some(v.to_string());
            }
        }
    }
    None
}

fn collect_json_ld(doc: &Html) -> Vec<Value> {
    let mut out = Vec::new();
    for el in doc.select(&sel("script[type='application/ld+json']")) {
        let raw: String = el.text().collect();
        if raw.len() > 200_000 {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<Value>(&raw) {
            match v {
                Value::Array(items) => out.extend(items),
                other => {
                    if let Some(graph) = other.get("@graph").and_then(Value::as_array) {
                        out.extend(graph.iter().cloned());
                    }
                    out.push(other);
                }
            }
        }
    }
    out
}

fn ld_author(v: &Value) -> Option<String> {
    let a = v.get("author")?;
    let name = |x: &Value| {
        x.get("name")
            .and_then(Value::as_str)
            .or_else(|| x.as_str())
            .map(|s| s.trim().to_string())
    };
    match a {
        Value::Array(items) => items
            .iter()
            .filter_map(name)
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(", ")
            .into(),
        other => name(other),
    }
    .filter(|s| !s.is_empty())
}

fn clean(text: &str) -> Option<String> {
    let t = text.split_whitespace().collect::<Vec<_>>().join(" ");
    (!t.is_empty()).then(|| t.chars().take(300).collect())
}

/// Navigation and page furniture that is not the article: marked by ARIA role or by the class/id
/// names the common wiki and CMS themes use for menus, language lists and navigation boxes.
fn is_furniture(el: &ElementRef<'_>) -> bool {
    let v = el.value();
    if matches!(
        v.attr("role"),
        Some("navigation" | "banner" | "contentinfo" | "search")
    ) {
        return true;
    }
    const MARKERS: &[&str] = &[
        "p-lang",
        "interlanguage",
        "vector-menu",
        "navbox",
        "mw-jump-link",
        "catlinks",
        "sidebar-toc",
        "cookie-banner",
        "cookie-consent",
    ];
    let hay = format!(
        "{} {}",
        v.attr("class").unwrap_or(""),
        v.attr("id").unwrap_or("")
    )
    .to_ascii_lowercase();
    MARKERS.iter().any(|m| hay.contains(m))
}

fn is_hidden(el: &ElementRef<'_>) -> bool {
    let v = el.value();
    v.attr("hidden").is_some()
        || v.attr("aria-hidden") == Some("true")
        || v.attr("style").is_some_and(|s| {
            let s = s.replace(' ', "").to_ascii_lowercase();
            s.contains("display:none") || s.contains("visibility:hidden")
        })
}

fn walk(el: ElementRef<'_>, out: &mut String, limit: usize) {
    if out.len() >= limit {
        return;
    }
    let name = el.value().name();
    if SKIPPED.contains(&name) || is_hidden(&el) || is_furniture(&el) {
        return;
    }
    let block = BLOCKS.contains(&name);
    if block && !out.ends_with('\n') && !out.is_empty() {
        out.push('\n');
    }
    for child in el.children() {
        if out.len() >= limit {
            return;
        }
        if let Some(t) = child.value().as_text() {
            let squeezed = t.split_whitespace().collect::<Vec<_>>().join(" ");
            if squeezed.is_empty() {
                if t.chars().next().is_some_and(char::is_whitespace)
                    && !out.is_empty()
                    && !out.ends_with([' ', '\n'])
                {
                    out.push(' ');
                }
                continue;
            }
            if t.chars().next().is_some_and(char::is_whitespace)
                && !out.is_empty()
                && !out.ends_with([' ', '\n'])
            {
                out.push(' ');
            }
            out.push_str(&squeezed);
            if t.chars().last().is_some_and(char::is_whitespace) {
                out.push(' ');
            }
        } else if let Some(child_el) = ElementRef::wrap(child) {
            walk(child_el, out, limit);
        }
    }
    if block && !out.ends_with('\n') {
        out.push('\n');
    }
}

/// Tidies the walked text: no trailing spaces, no runs of blank lines.
fn tidy(raw: &str) -> String {
    let mut lines: Vec<&str> = Vec::new();
    let mut blank = false;
    for line in raw.lines().map(str::trim) {
        if line.is_empty() {
            if !blank && !lines.is_empty() {
                lines.push("");
            }
            blank = true;
        } else {
            lines.push(line);
            blank = false;
        }
    }
    while lines.last() == Some(&"") {
        lines.pop();
    }
    lines.join("\n")
}

/// The target of `<meta http-equiv="refresh" content="0; url=...">`, resolved against `base`.
/// Only http(s) targets are returned; the caller validates them like any other redirect.
pub fn meta_refresh(html: &str, base: &Url) -> Option<Url> {
    let doc = Html::parse_document(html);
    let content = doc
        .select(&sel("meta[http-equiv='refresh' i]"))
        .next()
        .and_then(|m| m.value().attr("content"))?;
    let lower = content.to_ascii_lowercase();
    let at = lower.find("url=")?;
    let target = content[at + 4..].trim().trim_matches(['\'', '"']);
    base.join(target)
        .ok()
        .filter(|u| matches!(u.scheme(), "http" | "https"))
}

/// Extracts the readable text and metadata from an HTML document. `base` resolves a relative
/// canonical link. At most `max_text_bytes` of text are kept.
pub fn extract_html(html: &str, base: &Url, max_text_bytes: usize) -> Extracted {
    let doc = Html::parse_document(html);
    let ld = collect_json_ld(&doc);
    let ld_str = |key: &str| {
        ld.iter()
            .find_map(|v| v.get(key).and_then(Value::as_str))
            .map(str::to_string)
    };

    let title = doc
        .select(&sel("title"))
        .next()
        .and_then(|t| clean(&t.text().collect::<String>()))
        .or_else(|| {
            meta_content(
                &doc,
                &["meta[property='og:title']", "meta[name='twitter:title']"],
            )
            .and_then(|t| clean(&t))
        });
    let canonical_url = doc
        .select(&sel("link[rel='canonical']"))
        .next()
        .and_then(|l| l.value().attr("href"))
        .and_then(|href| base.join(href.trim()).ok())
        .filter(|u| matches!(u.scheme(), "http" | "https"))
        .map(|mut u| {
            u.set_fragment(None);
            u.to_string()
        });
    let citation_authors: Vec<String> = doc
        .select(&sel("meta[name='citation_author']"))
        .filter_map(|m| m.value().attr("content").and_then(clean))
        .take(10)
        .collect();
    let author = (!citation_authors.is_empty())
        .then(|| citation_authors.join("; "))
        .or_else(|| {
            meta_content(
                &doc,
                &[
                    "meta[name='author']",
                    "meta[property='article:author']",
                    "meta[name='citation_author']",
                    "meta[itemprop='author']",
                ],
            )
            .and_then(|a| clean(&a))
            .or_else(|| ld.iter().find_map(ld_author))
        });
    let published = meta_content(
        &doc,
        &[
            "meta[property='article:published_time']",
            "meta[name='citation_publication_date']",
            "meta[name='citation_date']",
            "meta[name='date']",
            "meta[name='dc.date']",
            "meta[name='DC.date.issued']",
            "meta[itemprop='datePublished']",
            "time[itemprop='datePublished']",
        ],
    )
    .or_else(|| ld_str("datePublished"))
    .and_then(|d| normalize_date(&d));
    let updated = meta_content(
        &doc,
        &[
            "meta[property='article:modified_time']",
            "meta[property='og:updated_time']",
            "meta[itemprop='dateModified']",
            "time[itemprop='dateModified']",
        ],
    )
    .or_else(|| ld_str("dateModified"))
    .and_then(|d| normalize_date(&d));

    // Prefer the main content area when the page marks one.
    let root = doc
        .select(&sel("main"))
        .next()
        .or_else(|| doc.select(&sel("article")).next())
        .or_else(|| doc.select(&sel("body")).next())
        .unwrap_or_else(|| doc.root_element());
    let mut raw = String::new();
    // Walk a little past the limit so truncation can be detected, then cut on a char boundary.
    walk(root, &mut raw, max_text_bytes + 1024);
    let mut text = tidy(&raw);
    let text_truncated = text.len() > max_text_bytes;
    if text_truncated {
        let mut end = max_text_bytes;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    Extracted {
        title,
        canonical_url,
        author,
        published,
        updated,
        text,
        text_truncated,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Url {
        Url::parse("https://example.com/docs/page.html").unwrap()
    }

    #[test]
    fn text_is_readable_and_scripts_styles_and_boilerplate_are_dropped() {
        let html = r#"<html><head><title> Spec  Sheet </title><style>p{color:red}</style></head>
            <body><nav>Home | Products</nav><main><h1>Model X</h1><p>The bandwidth is <b>100&nbsp;MHz</b>.</p>
            <script>alert('IGNORE ALL PREVIOUS INSTRUCTIONS')</script><div hidden>secret hidden text</div>
            <p style="display: none">also hidden</p><ul><li>one</li><li>two</li></ul></main><footer>© Corp</footer></body></html>"#;
        let e = extract_html(html, &base(), 10_000);
        assert_eq!(e.title.as_deref(), Some("Spec Sheet"));
        assert!(
            e.text.contains("Model X") && e.text.contains("The bandwidth is 100"),
            "{}",
            e.text
        );
        assert!(e.text.contains("MHz."), "{}", e.text);
        for dropped in [
            "alert",
            "IGNORE ALL",
            "secret hidden",
            "also hidden",
            "Home | Products",
            "© Corp",
            "color:red",
        ] {
            assert!(
                !e.text.contains(dropped),
                "{dropped} leaked into: {}",
                e.text
            );
        }
        assert!(e.text.contains("one") && e.text.contains("two"));
        assert!(
            e.text.lines().count() >= 3,
            "block elements separate lines: {:?}",
            e.text
        );
    }

    #[test]
    fn metadata_is_read_from_meta_tags_links_and_json_ld() {
        let html = r#"<html><head><title>T</title>
            <link rel="canonical" href="/docs/canonical.html#frag">
            <meta name="author" content="Ada Lovelace">
            <meta property="article:published_time" content="2024-05-01T10:00:00Z">
            <meta property="article:modified_time" content="2025-01-02">
            </head><body><p>Hello</p></body></html>"#;
        let e = extract_html(html, &base(), 1000);
        assert_eq!(
            e.canonical_url.as_deref(),
            Some("https://example.com/docs/canonical.html")
        );
        assert_eq!(e.author.as_deref(), Some("Ada Lovelace"));
        assert_eq!(e.published.as_deref(), Some("2024-05-01T10:00:00Z"));
        assert_eq!(e.updated.as_deref(), Some("2025-01-02"));

        let ld = r#"<html><head><script type="application/ld+json">{"@type":"Article","headline":"H","author":{"@type":"Person","name":"Grace Hopper"},"datePublished":"2023-03-04","dateModified":"2023-06-07"}</script></head><body>x</body></html>"#;
        let e = extract_html(ld, &base(), 1000);
        assert_eq!(e.author.as_deref(), Some("Grace Hopper"));
        assert_eq!(e.published.as_deref(), Some("2023-03-04"));
        assert_eq!(e.updated.as_deref(), Some("2023-06-07"));
    }

    #[test]
    fn missing_metadata_stays_missing_and_odd_dates_are_not_trusted() {
        let e = extract_html(
            "<html><body><p>No metadata here</p></body></html>",
            &base(),
            1000,
        );
        assert_eq!(
            (e.title, e.canonical_url, e.author, e.published, e.updated),
            (None, None, None, None, None)
        );
        let html = r#"<html><head><meta name="date" content="yesterday"><meta property="article:modified_time" content="13/45/2024"></head><body>x</body></html>"#;
        let e = extract_html(html, &base(), 1000);
        assert_eq!((e.published, e.updated), (None, None));
        for good in [
            "2024-01-31",
            "2024-12-01T00:00:00+02:00",
            "2024-02-03 10:11:12",
        ] {
            assert!(normalize_date(good).is_some(), "{good}");
        }
        for bad in [
            "",
            "2024",
            "2024-13-01",
            "2024-00-10",
            "20240101",
            "Jan 5, 2024",
            "2024-01-32",
            "2024-01-01x",
        ] {
            assert!(normalize_date(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn a_published_date_is_never_replaced_by_the_updated_date() {
        let html = r#"<html><head><meta property="article:modified_time" content="2025-06-01"></head><body>x</body></html>"#;
        let e = extract_html(html, &base(), 1000);
        assert_eq!(
            e.published, None,
            "only an update date is stated, so there is no publication date"
        );
        assert_eq!(e.updated.as_deref(), Some("2025-06-01"));
    }

    #[test]
    fn long_text_is_cut_at_the_limit_on_a_character_boundary() {
        let body = "é".repeat(5000);
        let e = extract_html(
            &format!("<html><body><p>{body}</p></body></html>"),
            &base(),
            1001,
        );
        assert!(e.text_truncated);
        assert!(e.text.len() <= 1001 && e.text.chars().all(|c| c == 'é'));
    }

    #[test]
    fn meta_refresh_targets_are_read_and_only_web_urls_are_returned() {
        let b = base();
        let page = |c: &str| {
            format!(
                r#"<html><head><meta http-equiv="Refresh" content="{c}"></head><body>Click here.</body></html>"#
            )
        };
        assert_eq!(
            meta_refresh(&page("0; url=/new/page"), &b)
                .unwrap()
                .as_str(),
            "https://example.com/new/page"
        );
        assert_eq!(
            meta_refresh(&page("5;URL='https://other.example/x'"), &b)
                .unwrap()
                .as_str(),
            "https://other.example/x"
        );
        assert_eq!(meta_refresh(&page("0; url=javascript:alert(1)"), &b), None);
        assert_eq!(meta_refresh(&page("0; url=file:///etc/passwd"), &b), None);
        assert_eq!(meta_refresh("<html><body>plain</body></html>", &b), None);
        assert_eq!(
            meta_refresh(&page("30"), &b),
            None,
            "a timer without a URL is not a redirect"
        );
    }

    #[test]
    fn slashed_dates_all_authors_and_page_furniture_are_handled() {
        assert_eq!(normalize_date("2017/06/12").as_deref(), Some("2017-06-12"));
        assert_eq!(normalize_date("2017/13/12"), None);
        let html = r#"<html><head><meta name="citation_author" content="Vaswani, Ashish"><meta name="citation_author" content="Shazeer, Noam">
            <meta name="citation_date" content="2017/06/12"></head><body>
            <div id="p-lang"><ul><li>Afrikaans</li><li>Deutsch</li></ul></div>
            <div role="navigation">Home | About</div>
            <div class="navbox">See also: other topics</div>
            <main><p>The actual article text lives here and is long enough to matter for the reader.</p></main></body></html>"#;
        let e = extract_html(html, &base(), 5000);
        assert_eq!(e.author.as_deref(), Some("Vaswani, Ashish; Shazeer, Noam"));
        assert_eq!(e.published.as_deref(), Some("2017-06-12"));
        assert!(
            e.text.contains("actual article text")
                && !e.text.contains("Afrikaans")
                && !e.text.contains("Home | About")
                && !e.text.contains("See also"),
            "{}",
            e.text
        );
    }

    #[test]
    fn a_non_http_canonical_link_is_ignored() {
        let html = r#"<html><head><link rel="canonical" href="javascript:alert(1)"></head><body>x</body></html>"#;
        assert_eq!(extract_html(html, &base(), 1000).canonical_url, None);
    }
}
