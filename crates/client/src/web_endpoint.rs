//! Which WebTransport server the browser build dials (`netcode_web.rs`).
//! Player-facing joins come from login tickets; a localhost debug page may
//! additionally name a local development server with `?dev_server=`.
//!
//! - A real hostname (`game.errantry.renownandruin.com`, optionally
//!   `host:port`, a pasted `https://host:port/` is fine too) is a deployment
//!   (`just deploy`): the game server holds a real certificate for that
//!   hostname, so the browser connects normally — no hash pinning — to
//!   `https://<host>:<port>/`, the port defaulting to
//!   [`DEFAULT_WEBTRANSPORT_PORT`]. A bare IP works only if the certificate
//!   names it; Let's Encrypt's do not, which is why the deployed build
//!   prefills the hostname.
//! - A local address (`127.0.0.1`, `localhost`, `[::1]`, empty) is the dev
//!   server on this machine (`just server` + `just web`): fetch
//!   `assets/webtransport-cert-hash.json` for its self-signed certificate's
//!   hash and port, and dial `127.0.0.1`.
//!
/// The server's WebTransport port (`server::WEBTRANSPORT_PORT`; the client does
/// not depend on the server crate).
pub const DEFAULT_WEBTRANSPORT_PORT: u16 = 6467;

/// The development server's loopback address.
pub const LOCAL_SERVER_ADDRESS: &str = "127.0.0.1";
/// The localhost-only debug query parameter used by `just web`.
pub const DEV_SERVER_QUERY: &str = "dev_server";

/// How to reach the server.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum WebServer {
    /// A trusted certificate: dial this URL as is.
    Trusted { url: String },
    /// The local dev server: pin the hash from the hash file, dial 127.0.0.1.
    PinnedLocal,
    /// A login ticket's world on a self-signed certificate (a dev host): dial
    /// `url` trusting exactly the SHA-256 `cert_hash` (hex) the ticket names.
    Pinned { url: String, cert_hash: String },
}

pub(crate) fn is_local_host(host: &str) -> bool {
    host.is_empty()
        || host == "localhost"
        || host.ends_with(".localhost")
        || host == "[::1]"
        || host == "::1"
        || host.starts_with("127.")
}

/// Splits `host[:port]` (`[v6]:port` too) after stripping a scheme and path.
/// `Err` names what is wrong with it.
fn split_address(address: &str) -> Result<(&str, Option<u16>), String> {
    let mut rest = address.trim();
    for scheme in ["https://", "http://"] {
        if let Some(stripped) = rest.strip_prefix(scheme) {
            rest = stripped;
        }
    }
    let rest = rest.split('/').next().unwrap_or("");
    let (host, port) = if let Some(v6) = rest.strip_prefix('[') {
        let Some(end) = v6.find(']') else {
            return Err(format!("bad address {address:?}"));
        };
        let host = &rest[..end + 2];
        (host, rest[end + 2..].strip_prefix(':'))
    } else {
        match rest.rsplit_once(':') {
            Some((host, port)) if !host.contains(':') => (host, Some(port)),
            Some(_) => (rest, None), // a bare IPv6 address
            None => (rest, None),
        }
    };
    let port = match port {
        None => None,
        Some(p) => match p.parse::<u16>() {
            Ok(0) | Err(_) => return Err(format!("bad port {p:?} in {address:?}")),
            Ok(p) => Some(p),
        },
    };
    if host.chars().any(char::is_whitespace) {
        return Err(format!("bad address {address:?}"));
    }
    Ok((host, port))
}

/// The localhost development address named by `?dev_server=`.
pub fn web_server_for(address: &str) -> Result<WebServer, String> {
    let (host, port) = split_address(address)?;
    if is_local_host(host) {
        return Ok(WebServer::PinnedLocal);
    }
    let port = port.unwrap_or(DEFAULT_WEBTRANSPORT_PORT);
    Ok(WebServer::Trusted {
        url: format!("https://{host}:{port}/"),
    })
}

/// The server a login ticket names (`JoinTicket::webtransport` +
/// `cert_hash`): its URL as is, pinned to the hash when it has one (a dev host's
/// self-signed certificate). Without a hash, a local URL is the dev server on
/// this machine, whose self-signed certificate changes every start (so the
/// registry cannot hold its hash — `just dev-login` registers none): it is
/// pinned from the dev hash file exactly like a typed local address
/// ([`WebServer::PinnedLocal`]); any other URL is trusted. `Err` names what is
/// wrong.
pub fn web_server_for_ticket(url: &str, cert_hash: Option<&str>) -> Result<WebServer, String> {
    let url = webtransport_url(url)?;
    match pinned(url, cert_hash)? {
        Some(server) => Ok(server),
        None if is_local_host(split_address(url)?.0) => Ok(WebServer::PinnedLocal),
        None => Ok(WebServer::Trusted { url: url.into() }),
    }
}

/// Another host of the world, as the home names it (`ServerPacket::Host` /
/// `RegionElsewhere`: R3, one WebTransport session per host): its URL pinned to ITS
/// `cert_hash` (the self-signed certificate it minted at start); without a hash (a
/// certificate from files) normal certificate validation — never the dev hash file,
/// which names only host 0. `Err` names what is wrong.
pub fn web_server_for_host(endpoint: &game::HostEndpoint) -> Result<WebServer, String> {
    let url = endpoint
        .webtransport
        .as_deref()
        .ok_or_else(|| format!("host {} offers no WebTransport address", endpoint.id))?;
    let url = webtransport_url(url)?;
    Ok(pinned(url, endpoint.cert_hash.as_deref())?
        .unwrap_or_else(|| WebServer::Trusted { url: url.into() }))
}

/// `url` trimmed, if it is an https URL.
fn webtransport_url(url: &str) -> Result<&str, String> {
    let url = url.trim();
    match url.starts_with("https://") {
        true => Ok(url),
        false => Err(format!("not a WebTransport URL: {url:?}")),
    }
}

/// `url` pinned to `cert_hash` (a hex SHA-256); `None` when there is no hash.
fn pinned(url: &str, cert_hash: Option<&str>) -> Result<Option<WebServer>, String> {
    match cert_hash.map(str::trim).filter(|h| !h.is_empty()) {
        None => Ok(None),
        Some(hash) if hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit()) => {
            Ok(Some(WebServer::Pinned {
                url: url.into(),
                cert_hash: hash.to_ascii_lowercase(),
            }))
        }
        Some(hash) => Err(format!("bad certificate hash {hash:?}")),
    }
}

fn query_value<'a>(search: &'a str, name: &str) -> Option<&'a str> {
    search
        .trim_start_matches('?')
        .split('&')
        .find_map(|pair| pair.strip_prefix(name)?.strip_prefix('='))
}

fn percent_decode(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = bytes.get(i + 1..i + 3)?;
            let high = (hex[0] as char).to_digit(16)? as u8;
            let low = (hex[1] as char).to_digit(16)? as u8;
            decoded.push((high << 4) | low);
            i += 3;
        } else {
            decoded.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

/// A local debug server supplied by the page URL. A release bundle and any
/// non-loopback page ignore it; the supplied value is never logged.
pub fn dev_server_from(search: &str, hostname: Option<&str>, debug_build: bool) -> Option<String> {
    use std::sync::atomic::{AtomicBool, Ordering};
    static WARNED: AtomicBool = AtomicBool::new(false);

    let supplied = query_value(search, DEV_SERVER_QUERY)?;
    let allowed = debug_build && crate::login::api::login_override_allowed(hostname);
    if !allowed {
        if !WARNED.swap(true, Ordering::Relaxed) {
            bevy::log::warn!("[web] ignoring ?{DEV_SERVER_QUERY}= (debug localhost pages only)");
        }
        return None;
    }
    percent_decode(supplied)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}
