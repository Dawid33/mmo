//! Where a dialer dials: `host:port`, an IP address or a DNS name.

use std::fmt;
use std::io;
use std::net::SocketAddr;

/// `host:port` — an IP address or a DNS name, or several comma-separated. A name is resolved
/// at every dial attempt, so a peer that comes back on another address (a restarted pod) is
/// found again; a dialer tries every address in turn (the replicas of a service).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkTarget(String);

impl LinkTarget {
    /// One or more comma-separated parts (the replicas of a service, e.g. in dev
    /// `127.0.0.1:6480,127.0.0.1:6481`), each a non-empty host and a `u16` port, split at
    /// the last `:`.
    pub fn parse(value: &str) -> Result<Self, String> {
        let parts = value.split(',').map(|p| Self::parse_part(p.trim())).collect::<Result<Vec<_>, _>>()?;
        Ok(Self(parts.join(",")))
    }

    fn parse_part(value: &str) -> Result<String, String> {
        if value.parse::<SocketAddr>().is_ok() {
            return Ok(value.to_string());
        }
        let (host, port) = value.rsplit_once(':').ok_or_else(|| format!("{value:?}: expected host:port"))?;
        if host.is_empty() {
            return Err(format!("{value:?}: the host is empty (expected host:port)"));
        }
        port.parse::<u16>().map_err(|_| format!("{value:?}: the port is not a number up to 65535 (expected host:port)"))?;
        Ok(value.to_string())
    }

    /// Every address to dial: each comma-separated part in order, an IP parsed as is and a
    /// name by the system resolver with every answer it gives, in answer order. A part that
    /// does not resolve is skipped while another does; none resolving is the first error.
    pub async fn resolve_all(&self) -> io::Result<Vec<SocketAddr>> {
        let mut all = Vec::new();
        let mut first_error = None;
        for part in self.0.split(',') {
            if let Ok(addr) = part.parse::<SocketAddr>() {
                all.push(addr);
                continue;
            }
            match tokio::net::lookup_host(part).await {
                Ok(answers) => {
                    let before = all.len();
                    all.extend(answers);
                    if all.len() == before {
                        first_error.get_or_insert_with(|| {
                            io::Error::new(io::ErrorKind::NotFound, format!("{part} resolved to no address"))
                        });
                    }
                }
                Err(e) => {
                    first_error.get_or_insert(e);
                }
            }
        }
        match (all.is_empty(), first_error) {
            (true, Some(e)) => Err(e),
            _ => Ok(all),
        }
    }

    /// The first address of [`Self::resolve_all`].
    pub async fn resolve(&self) -> io::Result<SocketAddr> {
        self.resolve_all()
            .await?
            .into_iter()
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
    async fn a_comma_list_resolves_every_part() {
        let target = LinkTarget::parse("127.0.0.1:6480, [::1]:6481 ,localhost:6482").unwrap();
        let all = target.resolve_all().await.unwrap();
        assert_eq!(&all[..2], ["127.0.0.1:6480".parse::<SocketAddr>().unwrap(), "[::1]:6481".parse().unwrap()]);
        assert!(all.len() >= 3 && all[2..].iter().all(|a| a.port() == 6482), "{all:?}");
        assert_eq!(target.to_string(), "127.0.0.1:6480, [::1]:6481 ,localhost:6482".replace(' ', ""));
        assert!(LinkTarget::parse("127.0.0.1:6480,").is_err(), "an empty part is refused");
        assert!(LinkTarget::parse("127.0.0.1:6480,host").is_err(), "every part is checked");
    }

    #[tokio::test]
    async fn localhost_resolves() {
        assert_eq!(LinkTarget::parse("localhost:6480").unwrap().resolve().await.unwrap().port(), 6480);
    }
}
