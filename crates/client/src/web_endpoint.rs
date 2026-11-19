//! Which WebTransport server the browser build dials (`netcode_web.rs`), from
//! the address the player typed on the main menu's Join server row. Pure and
//! target-independent so it is unit-tested natively.
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
//! The field is prefilled by [`default_server_address`]: the server the
//! bundle was built for (`VOXEL_WEB_SERVER` at build time — `just web-release`
//! / `just deploy` set it to `web_server` in the justfile), else the page's own
//! host when it is served over https, else `127.0.0.1`.

/// The server's WebTransport port (`server::WEBTRANSPORT_PORT`; the client does
/// not depend on the server crate).
pub const DEFAULT_WEBTRANSPORT_PORT: u16 = 6467;

/// The build-time default server (`VOXEL_WEB_SERVER`), if the bundle was
/// built for one.
pub const BUILT_FOR_SERVER: Option<&str> = option_env!("VOXEL_WEB_SERVER");

/// What the Join server field shows before the player types.
pub const LOCAL_SERVER_ADDRESS: &str = "127.0.0.1";

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
        let Some(end) = v6.find(']') else { return Err(format!("bad address {address:?}")) };
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

/// The server a typed address names.
pub fn web_server_for(address: &str) -> Result<WebServer, String> {
    let (host, port) = split_address(address)?;
    if is_local_host(host) {
        return Ok(WebServer::PinnedLocal);
    }
    let port = port.unwrap_or(DEFAULT_WEBTRANSPORT_PORT);
    Ok(WebServer::Trusted { url: format!("https://{host}:{port}/") })
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
    Ok(pinned(url, endpoint.cert_hash.as_deref())?.unwrap_or_else(|| WebServer::Trusted { url: url.into() }))
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
            Ok(Some(WebServer::Pinned { url: url.into(), cert_hash: hash.to_ascii_lowercase() }))
        }
        Some(hash) => Err(format!("bad certificate hash {hash:?}")),
    }
}

/// The Join server field's initial text: [`BUILT_FOR_SERVER`], else the
/// page's own host when served over https from a real hostname
/// (`location.protocol`, `location.hostname`), else [`LOCAL_SERVER_ADDRESS`].
pub fn default_server_address(built_for: Option<&str>, protocol: &str, hostname: &str) -> String {
    if let Some(server) = built_for.map(str::trim).filter(|s| !s.is_empty()) {
        return server.to_string();
    }
    if protocol == "https:" && !is_local_host(hostname) {
        return hostname.to_string();
    }
    LOCAL_SERVER_ADDRESS.to_string()
}

