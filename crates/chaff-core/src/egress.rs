//! The one place this application talks to the network.
//!
//! # Why a chokepoint and not a convention
//!
//! The PRD claimed for a long time that egress was "enforced through a single chokepoint
//! with an allowlist". It was not: `ureq` was called from three modules, each deciding its
//! own timeouts and its own error handling, and any of them could have been pointed
//! anywhere. A review found the claim was aspirational and stated as fact.
//!
//! This is the chokepoint. Every outbound request in the engine goes through
//! [`fetch`] or [`post`], and both check the destination against [`Policy`] first.
//!
//! # What the policy allows
//!
//! * **Loopback**, always. A model server on the same machine is not egress.
//! * **Private ranges** — `10/8`, `172.16/12`, `192.168/16`, link-local — because the
//!   user's stated setup is a 3090 on their own LAN, and refusing that would refuse the
//!   feature.
//! * **Named hosts**, explicitly. Model downloads are the only ones, and they are listed
//!   rather than pattern-matched: a suffix rule like `*.github.com` allows anything anyone
//!   can put on a subdomain.
//! * **Nothing else.** Not "deny by default with a bypass for convenience" — there is no
//!   bypass.
//!
//! # What this does not do
//!
//! It does not stop a request that has already been made, and it is not a firewall. A
//! determined process on the machine can still open a socket. What it does is make the
//! application's own egress **enumerable** — there is one list, and reading it tells you
//! everything this program will connect to.

use std::net::IpAddr;
use std::time::Duration;

/// A host this application is allowed to reach.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Destination {
    /// An IP address in a private or loopback range.
    Private,
    /// A hostname, named explicitly.
    Named(&'static str),
}

/// Every host the engine may reach by name.
///
/// **A list, not a pattern.** `*.github.com` would allow anything on any subdomain, which is
/// not an allowlist — it is a shape that looks like one.
pub const ALLOWED_HOSTS: &[&str] = &[
    "github.com",
    "objects.githubusercontent.com",
    "raw.githubusercontent.com",
    "huggingface.co",
    "cdn-lfs.huggingface.co",
];

#[derive(Debug, thiserror::Error)]
pub enum EgressError {
    #[error(
        "{host} is not on the egress allowlist. Chaff reaches loopback, private network \
         addresses, and the model-download hosts it names — nothing else. If this is a model \
         server, use its address on your own network."
    )]
    Blocked { host: String },
    #[error("could not parse {url} as a URL")]
    BadUrl { url: String },
    #[error("the request to {url} failed: {reason}")]
    Transport { url: String, reason: String },
    #[error("{url} answered {status}: {body}")]
    Status { url: String, status: u16, body: String },
}

/// What may be reached.
#[derive(Debug, Clone, Default)]
pub struct Policy {
    /// Extra hosts, for a user whose model lives at a name rather than an address.
    extra: Vec<String>,
    /// When true, the allowlist is not consulted at all.
    ///
    /// Only ever set by a test that needs a local server, and never reachable from the
    /// application: there is no command, setting or environment variable that turns it on.
    /// A bypass that can be enabled in the field is not a bypass, it is the design.
    unrestricted: bool,
}

impl Policy {
    /// A policy that also permits these hosts.
    pub fn with_hosts(hosts: impl IntoIterator<Item = String>) -> Self {
        Self { extra: hosts.into_iter().collect(), unrestricted: false }
    }

    /// Permit anything. **Tests only** — see the field's documentation.
    #[cfg(test)]
    pub fn unrestricted() -> Self {
        Self { extra: Vec::new(), unrestricted: true }
    }

    /// May this URL be reached?
    pub fn permits(&self, url: &str) -> Result<(), EgressError> {
        if self.unrestricted {
            return Ok(());
        }
        let host = host_of(url).ok_or_else(|| EgressError::BadUrl { url: url.to_string() })?;

        // A literal IP in a private or loopback range. The user's model server is on their
        // own LAN and refusing it would refuse the feature.
        if let Ok(ip) = host.parse::<IpAddr>() {
            return if is_private(ip) {
                Ok(())
            } else {
                Err(EgressError::Blocked { host })
            };
        }

        let lower = host.to_lowercase();
        let named = ALLOWED_HOSTS.iter().any(|h| *h == lower)
            || self.extra.iter().any(|h| h.to_lowercase() == lower);
        if named {
            return Ok(());
        }

        // `localhost` by name, which is loopback without being a literal address.
        if lower == "localhost" || lower.ends_with(".localhost") {
            return Ok(());
        }

        Err(EgressError::Blocked { host })
    }
}

/// The host part of a URL, without pulling in a URL parser for it.
fn host_of(url: &str) -> Option<String> {
    let rest = url.split_once("://")?.1;
    // Authority ends at the first `/`, `?` or `#`.
    let authority = rest.split(['/', '?', '#']).next()?;
    // Strip any userinfo, then any port.
    let host = authority.rsplit('@').next()?;
    let host = if host.starts_with('[') {
        // An IPv6 literal, e.g. `[::1]:8080`.
        host.split(']').next()?.trim_start_matches('[').to_string()
    } else {
        host.split(':').next()?.to_string()
    };
    (!host.is_empty()).then_some(host)
}

/// Loopback, link-local, or one of the private ranges.
///
/// The ranges `IpAddr::is_loopback` does not cover but that are still not the internet:
/// `10/8`, `172.16/12`, `192.168/16`, `169.254/16`, and their IPv6 equivalents.
fn is_private(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                // Carrier-grade NAT, which some home routers hand out.
                || (v4.octets()[0] == 100 && (64..128).contains(&v4.octets()[1]))
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                // Unique local addresses, `fc00::/7`.
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                // Link-local, `fe80::/10`.
                || (v6.segments()[0] & 0xffc0) == 0xfe80
        }
    }
}

/// GET a URL, if the policy permits it.
pub fn fetch(policy: &Policy, url: &str, timeout_secs: u64) -> Result<String, EgressError> {
    policy.permits(url)?;
    let response = ureq::get(url)
        .config()
        .timeout_global(Some(Duration::from_secs(timeout_secs)))
        .build()
        .call()
        .map_err(|e| EgressError::Transport { url: url.to_string(), reason: e.to_string() })?;
    response
        .into_body()
        .read_to_string()
        .map_err(|e| EgressError::Transport { url: url.to_string(), reason: e.to_string() })
}

/// POST a JSON body, if the policy permits it.
pub fn post(policy: &Policy, url: &str, body: &str, timeout_secs: u64) -> Result<String, EgressError> {
    policy.permits(url)?;
    let response = ureq::post(url)
        .config()
        .timeout_global(Some(Duration::from_secs(timeout_secs)))
        .build()
        .header("Content-Type", "application/json")
        .send(body)
        .map_err(|e| EgressError::Transport { url: url.to_string(), reason: e.to_string() })?;
    response
        .into_body()
        .read_to_string()
        .map_err(|e| EgressError::Transport { url: url.to_string(), reason: e.to_string() })
}

/// Download, reporting progress as it arrives.
///
/// The streaming counterpart of [`download`], for a file large enough that a silent minute
/// looks like a hang. Same policy check, same transport — the model store needs progress and
/// that is the only reason this exists as a second function rather than a flag.
pub fn download_with_progress(
    policy: &Policy,
    url: &str,
    timeout_secs: u64,
    on_progress: &mut dyn FnMut(u64, u64),
) -> Result<Vec<u8>, EgressError> {
    policy.permits(url)?;
    let response = ureq::get(url)
        .config()
        .timeout_global(Some(Duration::from_secs(timeout_secs)))
        .build()
        .call()
        .map_err(|e| EgressError::Transport { url: url.to_string(), reason: e.to_string() })?;

    let total = response
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);

    let mut body = Vec::with_capacity(total.min(64 * 1024 * 1024) as usize);
    let mut reader = response.into_body().into_reader();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = std::io::Read::read(&mut reader, &mut buf)
            .map_err(|e| EgressError::Transport { url: url.to_string(), reason: e.to_string() })?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&buf[..n]);
        on_progress(body.len() as u64, total);
    }
    Ok(body)
}

/// Download bytes, if the policy permits it.
///
/// The one caller is the model store, which verifies a hash before writing — so this
/// returns bytes rather than streaming to a file.
pub fn download(policy: &Policy, url: &str, timeout_secs: u64) -> Result<Vec<u8>, EgressError> {
    policy.permits(url)?;
    let response = ureq::get(url)
        .config()
        .timeout_global(Some(Duration::from_secs(timeout_secs)))
        .build()
        .call()
        .map_err(|e| EgressError::Transport { url: url.to_string(), reason: e.to_string() })?;

    let mut body = Vec::new();
    let mut reader = response.into_body().into_reader();
    std::io::Read::read_to_end(&mut reader, &mut body)
        .map_err(|e| EgressError::Transport { url: url.to_string(), reason: e.to_string() })?;
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_and_private_addresses_are_allowed() {
        // The user's stated setup is a 3090 on their own LAN. Refusing this would refuse the
        // feature, and a policy that blocks the intended use is one that gets turned off.
        let p = Policy::default();
        for url in [
            "http://127.0.0.1:8080/health",
            "http://localhost:8080/v1/models",
            "http://192.168.1.150:8080/v1/chat/completions",
            "http://10.0.0.5:1234/v1/models",
            "http://172.16.4.1:8000/health",
            "http://169.254.1.1/",
            "http://[::1]:8080/health",
            "http://[fd00::1]:8080/health",
        ] {
            assert!(p.permits(url).is_ok(), "should be allowed: {url}");
        }
    }

    #[test]
    fn a_public_address_is_refused() {
        // **The whole point.** A private range is the user's own network; a public address is
        // somebody else's, and reaching one is the thing the allowlist exists to prevent.
        let p = Policy::default();
        for url in [
            "http://8.8.8.8/",
            "https://1.1.1.1/",
            "http://172.32.0.1/",  // just outside 172.16/12
            "http://11.0.0.1/",    // just outside 10/8
            "http://192.169.1.1/", // just outside 192.168/16
        ] {
            assert!(p.permits(url).is_err(), "should be blocked: {url}");
        }
    }

    #[test]
    fn the_named_hosts_are_exactly_the_ones_needed_for_models() {
        let p = Policy::default();
        for url in [
            "https://github.com/opencv/opencv_zoo/raw/main/x.onnx",
            "https://huggingface.co/some/model",
        ] {
            assert!(p.permits(url).is_ok(), "should be allowed: {url}");
        }
    }

    #[test]
    fn a_lookalike_host_is_not_allowed() {
        // **The reason the list is exact rather than a suffix pattern.** `*.github.com`
        // would allow anything anyone can put on a subdomain, which is not an allowlist —
        // it is a shape that looks like one.
        let p = Policy::default();
        for url in [
            "https://github.com.evil.example/x",
            "https://notgithub.com/x",
            "https://raw.githubusercontent.com.evil.example/x",
            "https://evilgithub.com/x",
        ] {
            assert!(p.permits(url).is_err(), "should be blocked: {url}");
        }
    }

    #[test]
    fn an_unknown_host_is_refused() {
        let p = Policy::default();
        assert!(matches!(
            p.permits("https://example.com/"),
            Err(EgressError::Blocked { .. })
        ));
    }

    #[test]
    fn a_host_the_user_names_is_allowed() {
        // A model server reached by name rather than address — `gpu-box.local` — is a real
        // setup, and the policy takes the names it is given.
        let p = Policy::with_hosts(vec!["gpu-box.local".to_string()]);
        assert!(p.permits("http://gpu-box.local:8080/health").is_ok());
        assert!(p.permits("http://other.local:8080/health").is_err());
    }

    #[test]
    fn the_host_is_parsed_out_of_a_url_with_ports_and_credentials() {
        assert_eq!(host_of("http://host:8080/path"), Some("host".into()));
        assert_eq!(host_of("https://host/path?q=1"), Some("host".into()));
        assert_eq!(host_of("http://user:pass@host:1/x"), Some("host".into()));
        assert_eq!(host_of("http://[::1]:8080/x"), Some("::1".into()));
        assert_eq!(host_of("not a url"), None);
        assert_eq!(host_of("http://"), None);
    }

    #[test]
    fn the_blocked_message_says_what_to_do() {
        // A refusal that does not explain itself sends someone looking at firewalls. This
        // one names the host and the alternative.
        let e = Policy::default().permits("https://example.com/").unwrap_err();
        let m = e.to_string();
        assert!(m.contains("example.com"), "must name the host: {m}");
        assert!(m.contains("loopback"), "must say what *is* allowed: {m}");
    }

    #[test]
    fn every_allowed_host_is_a_bare_hostname() {
        // A pattern, a wildcard or a URL in this list would silently never match — an
        // allowlist entry that does nothing is worse than a missing one, because it reads
        // like it works.
        for h in ALLOWED_HOSTS {
            assert!(!h.contains('*'), "{h} is a pattern, not a host");
            assert!(!h.contains('/'), "{h} is a URL, not a host");
            assert_eq!(*h, h.to_lowercase(), "{h} must be lower case to match");
        }
    }
}
