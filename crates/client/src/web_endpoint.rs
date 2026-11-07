//! Which WebTransport server the browser build dials (`netcode_web.rs`), from
//! the address the player typed on the main menu's Join server row. Pure and
//! target-independent so it is unit-tested natively.
//!
//! - A real hostname (`orion.factoriosurvivalworld.com`, optionally
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hostname_dials_its_trusted_certificate() {
        let trusted = |url: &str| Ok(WebServer::Trusted { url: url.into() });
        assert_eq!(web_server_for("game.example.com"), trusted("https://game.example.com:6467/"));
        assert_eq!(web_server_for(" game.example.com:7000 "), trusted("https://game.example.com:7000/"));
        assert_eq!(web_server_for("https://game.example.com:7000/"), trusted("https://game.example.com:7000/"));
        assert_eq!(web_server_for("5.9.7.138"), trusted("https://5.9.7.138:6467/"));
        assert_eq!(web_server_for("[2001:db8::1]:7000"), trusted("https://[2001:db8::1]:7000/"));
    }

    #[test]
    fn local_addresses_pin_the_dev_server() {
        for local in ["127.0.0.1", "127.0.0.1:6467", "localhost", "[::1]", "::1", "", "http://localhost:1334/"] {
            assert_eq!(web_server_for(local), Ok(WebServer::PinnedLocal), "{local}");
        }
    }

    #[test]
    fn bad_ports_and_addresses_are_refused() {
        assert!(web_server_for("game.example.com:nope").is_err());
        assert!(web_server_for("game.example.com:0").is_err());
        assert!(web_server_for("game.example.com:70000").is_err());
        assert!(web_server_for("game example.com").is_err());
        assert!(web_server_for("[::1").is_err());
    }

    #[test]
    fn the_field_prefills_the_built_for_server_then_the_page_host() {
        let deployed = Some("orion.factoriosurvivalworld.com");
        assert_eq!(default_server_address(deployed, "http:", "127.0.0.1"), "orion.factoriosurvivalworld.com");
        assert_eq!(default_server_address(Some("  "), "https:", "game.example.com"), "game.example.com");
        assert_eq!(default_server_address(None, "https:", "game.example.com"), "game.example.com");
        assert_eq!(default_server_address(None, "http:", "game.example.com"), LOCAL_SERVER_ADDRESS);
        assert_eq!(default_server_address(None, "https:", "localhost"), LOCAL_SERVER_ADDRESS);
    }
}
