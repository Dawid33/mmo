//! Where a dialer dials: `host:port`, an IP address or a DNS name.

use std::fmt;
use std::io;
use std::net::SocketAddr;

/// `host:port` — an IP address or a DNS name. A name is resolved at every dial attempt, so a
/// peer that comes back on another address (a restarted pod) is found again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkTarget(String);

impl LinkTarget {
    /// A non-empty host and a `u16` port, split at the last `:`.
    pub fn parse(value: &str) -> Result<Self, String> {
        let value = value.trim();
        if value.parse::<SocketAddr>().is_ok() {
            return Ok(Self(value.to_string()));
        }
        let (host, port) = value.rsplit_once(':').ok_or_else(|| format!("{value:?}: expected host:port"))?;
        if host.is_empty() {
            return Err(format!("{value:?}: the host is empty (expected host:port)"));
        }
        port.parse::<u16>().map_err(|_| format!("{value:?}: the port is not a number up to 65535 (expected host:port)"))?;
        Ok(Self(value.to_string()))
    }

    /// The address to dial now: parsed when the host is an IP, else the first address the
    /// system resolver returns.
    pub async fn resolve(&self) -> io::Result<SocketAddr> {
        if let Ok(addr) = self.0.parse::<SocketAddr>() {
            return Ok(addr);
        }
        tokio::net::lookup_host(self.0.as_str())
            .await?
            .next()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("{} resolved to no address", self.0)))
    }
}

impl From<SocketAddr> for LinkTarget {
    fn from(addr: SocketAddr) -> Self {
        Self(addr.to_string())
    }
}

impl fmt::Display for LinkTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_accepts_ip_and_name() {
        assert!(LinkTarget::parse("127.0.0.1:6480").is_ok());
        assert!(LinkTarget::parse("[::1]:6480").is_ok());
        assert!(LinkTarget::parse("coordinator.world-errantry.svc.cluster.local:6480").is_ok());
    }

    #[test]
    fn parse_refuses_what_is_not_host_port() {
        for bad in ["", "coordinator", ":6480", "host:", "host:99999"] {
            assert!(LinkTarget::parse(bad).is_err(), "{bad}");
        }
    }

    #[tokio::test]
    async fn an_ip_resolves_without_a_lookup() {
        assert_eq!(
            LinkTarget::parse("127.0.0.1:6480").unwrap().resolve().await.unwrap(),
            "127.0.0.1:6480".parse().unwrap()
        );
    }

    #[tokio::test]
    async fn localhost_resolves() {
        assert_eq!(LinkTarget::parse("localhost:6480").unwrap().resolve().await.unwrap().port(), 6480);
    }
}
