//! Network policy for fetching public web pages: which URLs, hosts and addresses may be contacted.
//!
//! The fetcher exists to retrieve *public* pages. Everything that could reach the user's machine or
//! network (loopback, private ranges, link-local and cloud metadata addresses, internal host names)
//! is refused, both for the URL given and for every redirect hop. Host names are resolved by
//! [`SafeResolver`], which the HTTP client uses for the actual connection, so the address that was
//! checked is the address that is connected to (a name that resolves to a public address when
//! checked and to a private one when connecting is not possible).

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;

use async_trait::async_trait;
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use url::{Host, Url};

/// Longest URL accepted. Longer ones are refused: they can smuggle data to the destination.
pub const MAX_URL_BYTES: usize = 2048;

/// Why a fetch did not happen or did not produce a page. Every variant is a clear, user-visible
/// reason; none of them is ever treated as "verified".
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FetchError {
    #[error("not a valid URL: {0}")]
    InvalidUrl(String),
    #[error("the scheme `{0}` is not allowed (only public https{1} URLs are fetched)")]
    SchemeNotAllowed(String, &'static str),
    #[error("URLs with a user name or password are refused")]
    Credentials,
    #[error("port {0} is not allowed (only the standard web ports are used)")]
    PortNotAllowed(u16),
    #[error("{host} is refused: {reason}")]
    BlockedAddress { host: String, reason: String },
    #[error("could not resolve {host}: {detail}")]
    Dns { host: String, detail: String },
    #[error("timed out after {0}s")]
    Timeout(u64),
    #[error("more than {0} redirects")]
    TooManyRedirects(usize),
    #[error("redirect to {0} was refused: {1}")]
    RedirectRefused(String, String),
    #[error("HTTP status {0}")]
    HttpStatus(u16),
    #[error("unsupported content type `{0}` (only HTML and plain text are read)")]
    UnsupportedContentType(String),
    #[error("the page is larger than {0} bytes")]
    TooLarge(usize),
    #[error("blocked by the site's robots.txt")]
    RobotsDisallowed,
    #[error("robots.txt could not be read ({0}), so the site is treated as off limits")]
    RobotsUnavailable(String),
    #[error("network error: {0}")]
    Network(String),
    #[error("cancelled")]
    Cancelled,
}

impl FetchError {
    /// Short stable name for records and JSON.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::InvalidUrl(_) => "invalid_url",
            Self::SchemeNotAllowed(..) => "scheme_not_allowed",
            Self::Credentials => "credentials_in_url",
            Self::PortNotAllowed(_) => "port_not_allowed",
            Self::BlockedAddress { .. } => "blocked_address",
            Self::Dns { .. } => "dns",
            Self::Timeout(_) => "timeout",
            Self::TooManyRedirects(_) => "too_many_redirects",
            Self::RedirectRefused(..) => "redirect_refused",
            Self::HttpStatus(_) => "http_status",
            Self::UnsupportedContentType(_) => "unsupported_content_type",
            Self::TooLarge(_) => "too_large",
            Self::RobotsDisallowed => "robots_disallowed",
            Self::RobotsUnavailable(_) => "robots_unavailable",
            Self::Network(_) => "network",
            Self::Cancelled => "cancelled",
        }
    }
}

/// What may be fetched. The defaults are the safe ones; the relaxed switches exist for local test
/// servers and are never set from the config file or the command line.
#[derive(Debug, Clone)]
pub struct FetchPolicy {
    /// Allow `http://` as well as `https://`. Default false (`research.allow_http` can turn it on).
    pub allow_http: bool,
    /// One address treated as public (the loopback address of a local test server). Everything
    /// else stays checked, so redirects and DNS answers pointing elsewhere are still refused.
    /// For tests only; never set from the config file or the command line.
    #[doc(hidden)]
    pub test_allow_ip: Option<IpAddr>,
    /// Allow any port. For tests with a local server only.
    #[doc(hidden)]
    pub allow_any_port: bool,
    pub max_redirects: usize,
    pub timeout_secs: u64,
    pub max_page_bytes: usize,
    pub max_text_bytes: usize,
    pub max_concurrent: usize,
    pub respect_robots: bool,
    pub user_agent: String,
}

impl Default for FetchPolicy {
    fn default() -> Self {
        Self {
            allow_http: false,
            test_allow_ip: None,
            allow_any_port: false,
            max_redirects: 5,
            timeout_secs: 15,
            max_page_bytes: 2 * 1024 * 1024,
            max_text_bytes: 200 * 1024,
            max_concurrent: 4,
            respect_robots: true,
            user_agent: "ai-orchestra/0.6 (local research tool; respects robots.txt)".to_string(),
        }
    }
}

impl FetchPolicy {
    /// Relaxed policy for tests against a server on 127.0.0.1. Never used outside tests.
    #[doc(hidden)]
    pub fn for_local_tests() -> Self {
        Self {
            allow_http: true,
            test_allow_ip: Some(IpAddr::V4(Ipv4Addr::LOCALHOST)),
            allow_any_port: true,
            ..Self::default()
        }
    }

    /// Public-address check with the test exception applied.
    pub fn check_ip(&self, ip: IpAddr) -> Result<(), &'static str> {
        if self.test_allow_ip == Some(ip) {
            return Ok(());
        }
        check_public_ip(ip)
    }
}

/// `Ok` when `ip` is a public, globally routable address.
pub fn check_public_ip(ip: IpAddr) -> Result<(), &'static str> {
    match ip {
        IpAddr::V4(v4) => check_public_v4(v4),
        IpAddr::V6(v6) => check_public_v6(v6),
    }
}

fn check_public_v4(ip: Ipv4Addr) -> Result<(), &'static str> {
    let [a, b, c, _] = ip.octets();
    if ip.is_unspecified() || a == 0 {
        Err("the unspecified/\"this network\" address")
    } else if ip.is_loopback() {
        Err("a loopback address")
    } else if ip.is_private() {
        Err("a private network address")
    } else if ip.is_link_local() {
        Err("a link-local address (this range includes cloud metadata services)")
    } else if a == 100 && (64..=127).contains(&b) {
        Err("a carrier-grade NAT address")
    } else if ip.is_multicast() {
        Err("a multicast address")
    } else if ip.is_broadcast() || a >= 240 {
        Err("a reserved or broadcast address")
    } else if (a, b, c) == (192, 0, 0) {
        Err("an IETF protocol assignment address")
    } else if (a, b, c) == (192, 0, 2) || (a, b, c) == (198, 51, 100) || (a, b, c) == (203, 0, 113)
    {
        Err("a documentation address")
    } else if a == 198 && (b == 18 || b == 19) {
        Err("a benchmarking address")
    } else if (a, b, c) == (192, 88, 99) {
        Err("a 6to4 relay anycast address")
    } else {
        Ok(())
    }
}

fn check_public_v6(ip: Ipv6Addr) -> Result<(), &'static str> {
    // IPv4 hidden inside an IPv6 address must pass the IPv4 rules.
    if let Some(v4) = ip.to_ipv4_mapped() {
        return check_public_v4(v4);
    }
    let seg = ip.segments();
    if ip.is_unspecified() {
        Err("the unspecified address")
    } else if ip.is_loopback() {
        Err("a loopback address")
    } else if ip.is_multicast() {
        Err("a multicast address")
    } else if seg[0] & 0xfe00 == 0xfc00 {
        Err("a unique-local (private) address")
    } else if seg[0] & 0xffc0 == 0xfe80 {
        Err("a link-local address")
    } else if seg[0] & 0xffc0 == 0xfec0 {
        Err("a deprecated site-local address")
    } else if seg[0] == 0x2001 && seg[1] == 0x0db8 {
        Err("a documentation address")
    } else if seg[0] == 0x0064 && seg[1] == 0xff9b {
        // NAT64: the low 32 bits are an IPv4 address.
        let v4 = Ipv4Addr::new(
            (seg[6] >> 8) as u8,
            seg[6] as u8,
            (seg[7] >> 8) as u8,
            seg[7] as u8,
        );
        check_public_v4(v4).map_err(|_| "a NAT64 address that maps to a non-public IPv4 address")
    } else if seg[0] == 0x2002 {
        // 6to4: the next 32 bits are an IPv4 address.
        let v4 = Ipv4Addr::new(
            (seg[1] >> 8) as u8,
            seg[1] as u8,
            (seg[2] >> 8) as u8,
            seg[2] as u8,
        );
        check_public_v4(v4).map_err(|_| "a 6to4 address that maps to a non-public IPv4 address")
    } else if ip.to_ipv4().is_some() {
        Err("an IPv4-compatible address")
    } else {
        Ok(())
    }
}

/// Host names that never name a public web site.
pub fn check_host_name(host: &str) -> Result<(), &'static str> {
    let h = host.trim_end_matches('.').to_ascii_lowercase();
    if h.is_empty() {
        return Err("an empty host name");
    }
    let internal_suffixes = [
        ".localhost",
        ".local",
        ".localdomain",
        ".internal",
        ".intranet",
        ".lan",
        ".home",
        ".corp",
        ".private",
        ".home.arpa",
    ];
    if h == "localhost" || internal_suffixes.iter().any(|s| h.ends_with(s)) {
        Err("an internal host name")
    } else if h == "metadata" || h.starts_with("metadata.") {
        Err("a cloud metadata host name")
    } else if !h.contains('.') {
        Err("a host name without a dot (not a public domain)")
    } else {
        Ok(())
    }
}

/// Checks everything about `url` that can be checked without the network: scheme, credentials,
/// port, length, and (for IP literals and host names) the destination. Redirect targets go
/// through the same function.
pub fn validate_url(url: &Url, policy: &FetchPolicy) -> Result<(), FetchError> {
    if url.as_str().len() > MAX_URL_BYTES {
        return Err(FetchError::InvalidUrl(format!(
            "longer than {MAX_URL_BYTES} bytes"
        )));
    }
    match url.scheme() {
        "https" => {}
        "http" if policy.allow_http => {}
        other => {
            return Err(FetchError::SchemeNotAllowed(
                other.to_string(),
                if policy.allow_http { " and http" } else { "" },
            ));
        }
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(FetchError::Credentials);
    }
    if let Some(port) = url.port() {
        let standard = port == 80 || port == 443;
        if !standard && !policy.allow_any_port {
            return Err(FetchError::PortNotAllowed(port));
        }
    }
    let Some(host) = url.host() else {
        return Err(FetchError::InvalidUrl("no host".to_string()));
    };
    let blocked = |reason: &str| FetchError::BlockedAddress {
        host: url.host_str().unwrap_or("").to_string(),
        reason: reason.to_string(),
    };
    match host {
        Host::Ipv4(ip) => policy.check_ip(IpAddr::V4(ip)).map_err(blocked)?,
        Host::Ipv6(ip) => policy.check_ip(IpAddr::V6(ip)).map_err(blocked)?,
        Host::Domain(name) => check_host_name(name).map_err(blocked)?,
    }
    Ok(())
}

/// Parses and validates a URL given by a user or reported by a provider.
pub fn parse_public_url(text: &str, policy: &FetchPolicy) -> Result<Url, FetchError> {
    let url = Url::parse(text.trim()).map_err(|e| FetchError::InvalidUrl(e.to_string()))?;
    validate_url(&url, policy)?;
    Ok(url)
}

/// DNS lookup, abstracted so tests can script answers (including a name whose answer changes).
#[async_trait]
pub trait HostResolver: Send + Sync {
    async fn lookup(&self, host: &str, port: u16) -> std::io::Result<Vec<IpAddr>>;
}

/// The operating system's resolver.
#[derive(Debug, Default)]
pub struct SystemResolver;

#[async_trait]
impl HostResolver for SystemResolver {
    async fn lookup(&self, host: &str, port: u16) -> std::io::Result<Vec<IpAddr>> {
        let addrs = tokio::net::lookup_host((host, port)).await?;
        Ok(addrs.map(|a| a.ip()).collect())
    }
}

/// The resolver given to the HTTP client. It refuses a name if **any** of its addresses is
/// non-public (a mixed answer is a rebinding trick), and the client then connects to exactly the
/// addresses returned here, so there is no gap between "checked" and "used".
pub struct SafeResolver {
    inner: Arc<dyn HostResolver>,
    policy: FetchPolicy,
}

impl SafeResolver {
    pub fn new(inner: Arc<dyn HostResolver>, policy: &FetchPolicy) -> Self {
        Self {
            inner,
            policy: policy.clone(),
        }
    }
}

impl fmt::Debug for SafeResolver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SafeResolver")
    }
}

impl Resolve for SafeResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let inner = self.inner.clone();
        let policy = self.policy.clone();
        Box::pin(async move {
            let host = name.as_str().to_string();
            let ips = inner.lookup(&host, 0).await.map_err(|e| {
                Box::new(FetchError::Dns {
                    host: host.clone(),
                    detail: e.to_string(),
                }) as Box<dyn std::error::Error + Send + Sync>
            })?;
            if ips.is_empty() {
                return Err(Box::new(FetchError::Dns {
                    host,
                    detail: "no addresses".to_string(),
                })
                    as Box<dyn std::error::Error + Send + Sync>);
            }
            for ip in &ips {
                if let Err(reason) = policy.check_ip(*ip) {
                    return Err(Box::new(FetchError::BlockedAddress {
                        host: format!("{host} ({ip})"),
                        reason: reason.to_string(),
                    })
                        as Box<dyn std::error::Error + Send + Sync>);
                }
            }
            let addrs: Addrs = Box::new(
                ips.into_iter()
                    .map(|ip| SocketAddr::new(ip, 0))
                    .collect::<Vec<_>>()
                    .into_iter(),
            );
            Ok(addrs)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn public_addresses_are_accepted() {
        for a in [
            "93.184.216.34",
            "8.8.8.8",
            "1.1.1.1",
            "151.101.1.69",
            "2606:2800:220:1:248:1893:25c8:1946",
            "2a00:1450:4001:81b::200e",
        ] {
            assert!(check_public_ip(ip(a)).is_ok(), "{a} should be public");
        }
    }

    #[test]
    fn private_loopback_linklocal_metadata_and_reserved_addresses_are_refused() {
        for a in [
            "127.0.0.1",
            "127.255.255.254",
            "10.0.0.1",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.1",
            "169.254.169.254",
            "169.254.0.1",
            "100.64.0.1",
            "0.0.0.0",
            "0.1.2.3",
            "224.0.0.1",
            "239.255.255.255",
            "255.255.255.255",
            "240.0.0.1",
            "192.0.0.1",
            "192.0.2.1",
            "198.51.100.1",
            "203.0.113.1",
            "198.18.0.1",
            "::1",
            "::",
            "fe80::1",
            "fc00::1",
            "fd00:ec2::254",
            "ff02::1",
            "2001:db8::1",
            "::ffff:127.0.0.1",
            "::ffff:10.1.2.3",
            "::ffff:169.254.169.254",
            "64:ff9b::7f00:1",
            "2002:7f00:1::1",
            "fec0::1",
        ] {
            assert!(check_public_ip(ip(a)).is_err(), "{a} must be refused");
        }
        // 172.32.x is NOT private; 100.63 / 100.128 are outside CGNAT.
        for a in ["172.32.0.1", "100.63.0.1", "100.128.0.1", "198.20.0.1"] {
            assert!(check_public_ip(ip(a)).is_ok(), "{a} is public");
        }
    }

    #[test]
    fn internal_host_names_are_refused() {
        for h in [
            "localhost",
            "LOCALHOST",
            "foo.localhost",
            "printer.local",
            "db.internal",
            "metadata.google.internal",
            "metadata",
            "intranet",
            "router.lan",
            "x.home.arpa",
            "localhost.",
        ] {
            assert!(check_host_name(h).is_err(), "{h}");
        }
        for h in [
            "example.com",
            "docs.rust-lang.org",
            "a.b.example.org",
            "xn--e1afmkfd.xn--p1ai",
        ] {
            assert!(check_host_name(h).is_ok(), "{h}");
        }
    }

    #[test]
    fn urls_are_validated_by_scheme_credentials_port_and_destination() {
        let p = FetchPolicy::default();
        let ok = |u: &str| validate_url(&Url::parse(u).unwrap(), &p);
        assert!(ok("https://example.com/page").is_ok());
        assert!(ok("https://example.com:443/page").is_ok());
        assert!(matches!(
            ok("http://example.com/").unwrap_err(),
            FetchError::SchemeNotAllowed(..)
        ));
        for scheme in [
            "file:///etc/passwd",
            "ftp://example.com/x",
            "gopher://example.com/",
            "javascript:alert(1)",
            "data:text/html,hi",
        ] {
            assert!(ok(scheme).is_err(), "{scheme}");
        }
        assert_eq!(
            ok("https://user:pw@example.com/").unwrap_err(),
            FetchError::Credentials
        );
        assert_eq!(
            ok("https://user@example.com/").unwrap_err(),
            FetchError::Credentials
        );
        assert_eq!(
            ok("https://example.com:8443/").unwrap_err(),
            FetchError::PortNotAllowed(8443)
        );
        assert!(matches!(
            ok("https://127.0.0.1/").unwrap_err(),
            FetchError::BlockedAddress { .. }
        ));
        assert!(matches!(
            ok("https://[::1]/").unwrap_err(),
            FetchError::BlockedAddress { .. }
        ));
        assert!(matches!(
            ok("https://localhost/").unwrap_err(),
            FetchError::BlockedAddress { .. }
        ));
        assert!(matches!(
            ok("https://169.254.169.254/latest/meta-data/").unwrap_err(),
            FetchError::BlockedAddress { .. }
        ));
        // The URL parser normalizes disguised IPv4 spellings, so the check still sees them.
        for disguised in [
            "https://2130706433/",
            "https://0x7f.0.0.1/",
            "https://0177.0.0.1/",
            "https://127.1/",
            "https://[::ffff:7f00:1]/",
            "https://[::ffff:127.0.0.1]/",
        ] {
            assert!(
                matches!(
                    ok(disguised).unwrap_err(),
                    FetchError::BlockedAddress { .. }
                ),
                "{disguised}"
            );
        }
        let long = format!("https://example.com/{}", "a".repeat(MAX_URL_BYTES));
        assert!(matches!(ok(&long).unwrap_err(), FetchError::InvalidUrl(_)));
        assert!(parse_public_url("not a url", &p).is_err());
        assert!(parse_public_url("  https://example.com/x  ", &p).is_ok());
    }

    #[test]
    fn http_is_allowed_only_when_the_policy_says_so() {
        let p = FetchPolicy {
            allow_http: true,
            ..FetchPolicy::default()
        };
        assert!(validate_url(&Url::parse("http://example.com/").unwrap(), &p).is_ok());
        assert!(
            validate_url(&Url::parse("http://127.0.0.1/").unwrap(), &p).is_err(),
            "http does not unlock private addresses"
        );
    }

    struct Scripted(std::sync::Mutex<Vec<Vec<IpAddr>>>);

    #[async_trait]
    impl HostResolver for Scripted {
        async fn lookup(&self, _host: &str, _port: u16) -> std::io::Result<Vec<IpAddr>> {
            let mut answers = self.0.lock().unwrap();
            Ok(if answers.len() > 1 {
                answers.remove(0)
            } else {
                answers[0].clone()
            })
        }
    }

    async fn resolve(r: &SafeResolver, host: &str) -> Result<Vec<SocketAddr>, String> {
        let name: Name = host.parse().unwrap();
        r.resolve(name)
            .await
            .map(|a| a.collect())
            .map_err(|e| e.to_string())
    }

    #[tokio::test]
    async fn the_resolver_refuses_names_that_point_at_private_addresses() {
        let p = FetchPolicy::default();
        let r = SafeResolver::new(
            Arc::new(Scripted(std::sync::Mutex::new(vec![vec![ip("10.0.0.5")]]))),
            &p,
        );
        let err = resolve(&r, "evil.example").await.unwrap_err();
        assert!(err.contains("private network address"), "{err}");
        // A mixed answer (one public, one private) is refused as a whole.
        let r = SafeResolver::new(
            Arc::new(Scripted(std::sync::Mutex::new(vec![vec![
                ip("93.184.216.34"),
                ip("127.0.0.1"),
            ]]))),
            &p,
        );
        assert!(resolve(&r, "mixed.example").await.is_err());
        // A public answer passes through unchanged.
        let r = SafeResolver::new(
            Arc::new(Scripted(std::sync::Mutex::new(vec![vec![ip(
                "93.184.216.34",
            )]]))),
            &p,
        );
        assert_eq!(
            resolve(&r, "ok.example").await.unwrap(),
            vec![SocketAddr::new(ip("93.184.216.34"), 0)]
        );
    }

    #[tokio::test]
    async fn rebinding_is_caught_because_every_connection_resolves_and_checks_again() {
        // First answer public, second answer private: the second lookup is refused.
        let p = FetchPolicy::default();
        let r = SafeResolver::new(
            Arc::new(Scripted(std::sync::Mutex::new(vec![
                vec![ip("93.184.216.34")],
                vec![ip("169.254.169.254")],
            ]))),
            &p,
        );
        assert!(resolve(&r, "rebind.example").await.is_ok());
        assert!(resolve(&r, "rebind.example").await.is_err());
    }

    #[tokio::test]
    async fn the_test_policy_lets_the_resolver_return_only_its_one_loopback_address() {
        let p = FetchPolicy::for_local_tests();
        let r = SafeResolver::new(
            Arc::new(Scripted(std::sync::Mutex::new(vec![vec![ip("127.0.0.1")]]))),
            &p,
        );
        assert!(resolve(&r, "local.test").await.is_ok());
        let r = SafeResolver::new(
            Arc::new(Scripted(std::sync::Mutex::new(vec![vec![ip("10.0.0.1")]]))),
            &p,
        );
        assert!(
            resolve(&r, "other.test").await.is_err(),
            "the exception is for one address only"
        );
    }

    #[test]
    fn the_defaults_are_the_safe_ones() {
        let p = FetchPolicy::default();
        assert!(
            !p.allow_http && p.test_allow_ip.is_none() && !p.allow_any_port && p.respect_robots
        );
        assert!(
            p.max_redirects <= 10 && p.timeout_secs <= 60 && p.max_page_bytes <= 8 * 1024 * 1024
        );
    }
}
