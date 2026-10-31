//! The local server a menu-started world session plays on: the `server`
//! binary as a child process, serving exactly one world file (one world per
//! server process — `forged::install` is a `OnceLock`). Native only.
//!
//! The child is the binary next to the client executable, or
//! `VOXEL_SERVER_BIN`. It runs with `VOXEL_WORLD_FILE=<terrain>` and
//! `VOXEL_SINGLE_PLAYER=1` (it exits once its last client leaves) and writes
//! stdout + stderr to the world's server log. The port stays 6466 for now
//! (the server hard-codes it, and binds WebTransport 6467 next to it); a port
//! that is already taken is reported instead of guessed around.

use std::ffi::OsString;
use std::fs::File;
use std::io;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use bevy::prelude::Resource;

/// The QUIC port the server binds (hard-coded in `crates/server/src/lib.rs`).
pub const SERVER_PORT: u16 = 6466;

/// How long [`LocalHost::stop`] waits for the server to exit on its own (it
/// does once its last client has left) before killing it.
pub const STOP_GRACE: Duration = Duration::from_secs(5);

/// How often [`LocalHost::stop`] polls the child while it waits.
const STOP_POLL: Duration = Duration::from_millis(20);

/// Overrides the server binary [`server_binary`] finds.
pub const SERVER_BIN_ENV: &str = "VOXEL_SERVER_BIN";

/// The server's single-player switch (`server::SINGLE_PLAYER_ENV`; the client
/// does not depend on the server crate).
pub const SINGLE_PLAYER_ENV: &str = "VOXEL_SINGLE_PLAYER";

/// The error message when [`SERVER_PORT`] is taken.
pub const PORT_IN_USE: &str = "port 6466 is already in use (is a dev server running?)";

/// A running child server.
#[derive(Resource, Debug)]
pub struct LocalHost {
    child: Child,
    log: PathBuf,
    pub addr: SocketAddr,
}

/// `VOXEL_SERVER_BIN` if set, else `server` (`server.exe` on Windows) next to
/// the running executable.
pub fn server_binary() -> PathBuf {
    let exe = std::env::current_exe().unwrap_or_default();
    server_binary_from(std::env::var_os(SERVER_BIN_ENV), &exe)
}

/// [`server_binary`] over its inputs.
pub fn server_binary_from(override_bin: Option<OsString>, exe: &Path) -> PathBuf {
    if let Some(bin) = override_bin.filter(|b| !b.is_empty()) {
        return PathBuf::from(bin);
    }
    let name = if cfg!(windows) { "server.exe" } else { "server" };
    exe.parent().unwrap_or(Path::new(".")).join(name)
}

/// The server log of a world: `<entry dir>/server.log`, except the legacy
/// `worlds/default.world` (whose entry dir is the worlds root), which logs to
/// `worlds/default.server.log` beside it.
pub fn server_log_path(entry_dir: &Path, terrain: &Path) -> PathBuf {
    if terrain.file_name().is_some_and(|n| n == worldgen::world_store::TERRAIN_FILE) {
        entry_dir.join("server.log")
    } else {
        terrain.with_extension("server.log")
    }
}

fn server_addr() -> SocketAddr {
    SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, SERVER_PORT))
}

impl LocalHost {
    /// Start the server binary ([`server_binary`]) on `terrain`, logging to `log`.
    pub fn start(terrain: &Path, log: &Path) -> io::Result<LocalHost> {
        Self::start_with(&server_binary(), terrain, log)
    }

    /// [`start`](Self::start) with an explicit binary.
    pub fn start_with(binary: &Path, terrain: &Path, log: &Path) -> io::Result<LocalHost> {
        // QUIC is UDP: if we can't bind the port, neither can the child.
        if UdpSocket::bind(server_addr()).is_err() {
            return Err(io::Error::new(io::ErrorKind::AddrInUse, PORT_IN_USE));
        }
        if let Some(dir) = log.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let out = File::create(log)?;
        let err = out.try_clone()?;
        let child = Command::new(binary)
            .env("VOXEL_WORLD_FILE", terrain)
            .env(SINGLE_PLAYER_ENV, "1")
            .stdin(Stdio::null())
            .stdout(out)
            .stderr(err)
            .spawn()
            .map_err(|e| io::Error::new(e.kind(), format!("could not start {}: {e}", binary.display())))?;
        Ok(LocalHost { child, log: log.to_path_buf(), addr: server_addr() })
    }

    /// The child's exit status, if it has exited.
    pub fn try_exited(&mut self) -> Option<ExitStatus> {
        self.child.try_wait().ok().flatten()
    }

    /// Wait up to `grace` for the server to exit by itself, then kill it.
    /// Returns its exit status (`None` if it could not be reaped).
    pub fn stop(mut self, grace: Duration) -> Option<ExitStatus> {
        let deadline = Instant::now() + grace;
        loop {
            if let Some(status) = self.try_exited() {
                return Some(status);
            }
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            std::thread::sleep(STOP_POLL.min(deadline - now));
        }
        let _ = self.child.kill();
        self.child.wait().ok()
    }

    /// The last `lines` lines of the server log ("" if it can't be read).
    pub fn log_tail(&self, lines: usize) -> String {
        let text = std::fs::read_to_string(&self.log).unwrap_or_default();
        let all: Vec<&str> = text.lines().collect();
        all[all.len().saturating_sub(lines)..].join("\n")
    }

    /// The child's process id.
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// The server log's path.
    pub fn log_path(&self) -> &Path {
        &self.log
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Mutex;

    /// The tests all need port 6466: one at a time.
    static PORT: Mutex<()> = Mutex::new(());

    fn scratch(name: &str) -> PathBuf {
        let d = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/local-host-tests").join(name);
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A fake server: a shell script with `body`.
    fn script(dir: &Path, body: &str) -> PathBuf {
        let path = dir.join("fake-server.sh");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn wait_for(mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !done() {
            assert!(Instant::now() < deadline, "timed out");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn server_binary_is_next_to_the_exe_unless_overridden() {
        let exe = Path::new("/opt/game/client");
        let name = if cfg!(windows) { "server.exe" } else { "server" };
        assert_eq!(server_binary_from(None, exe), Path::new("/opt/game").join(name));
        assert_eq!(server_binary_from(Some("".into()), exe), Path::new("/opt/game").join(name));
        assert_eq!(server_binary_from(Some("/tmp/fake".into()), exe), PathBuf::from("/tmp/fake"));
    }

    #[test]
    fn legacy_world_logs_beside_its_file() {
        let root = Path::new("worlds");
        assert_eq!(
            server_log_path(&root.join("isle"), &root.join("isle/terrain.world")),
            root.join("isle/server.log")
        );
        assert_eq!(server_log_path(root, &root.join("default.world")), root.join("default.server.log"));
    }

    #[test]
    #[ignore = "slow: >100ms — run via just test-full"]
    fn start_passes_world_and_single_player_env() {
        let _port = PORT.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("env");
        let bin = script(&dir, "env; echo done");
        let log = dir.join("logs/server.log");
        let mut host = LocalHost::start_with(&bin, Path::new("/worlds/x/terrain.world"), &log).unwrap();
        assert_eq!(host.addr, server_addr());
        wait_for(|| host.try_exited().is_some());
        let text = std::fs::read_to_string(&log).unwrap();
        assert!(text.contains("VOXEL_WORLD_FILE=/worlds/x/terrain.world"), "{text}");
        assert!(text.contains("VOXEL_SINGLE_PLAYER=1"), "{text}");
        assert_eq!(host.log_tail(1), "done");
    }

    #[test]
    #[ignore = "slow: >100ms — run via just test-full"]
    fn stop_kills_after_grace() {
        let _port = PORT.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("stop");
        let bin = script(&dir, "trap '' TERM\nexec sleep 60");
        let mut host = LocalHost::start_with(&bin, Path::new("t.world"), &dir.join("server.log")).unwrap();
        let pid = host.child.id();
        assert!(host.try_exited().is_none());
        let started = Instant::now();
        let status = host.stop(Duration::from_millis(100));
        assert!(started.elapsed() < Duration::from_secs(1), "stop took {:?}", started.elapsed());
        assert!(status.is_some_and(|s| !s.success()), "{status:?}");
        assert!(!Path::new(&format!("/proc/{pid}")).exists(), "process {pid} still exists");
    }

    #[test]
    #[ignore = "slow: >100ms — run via just test-full"]
    fn stop_returns_early_when_the_server_exits() {
        let _port = PORT.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("early");
        let bin = script(&dir, "exit 0");
        let host = LocalHost::start_with(&bin, Path::new("t.world"), &dir.join("server.log")).unwrap();
        let started = Instant::now();
        let status = host.stop(STOP_GRACE);
        assert!(started.elapsed() < Duration::from_secs(2), "stop took {:?}", started.elapsed());
        assert!(status.is_some_and(|s| s.success()), "{status:?}");
    }

    #[test]
    #[ignore = "slow: >100ms — run via just test-full"]
    fn port_in_use_is_reported() {
        let _port = PORT.lock().unwrap_or_else(|e| e.into_inner());
        let _taken = UdpSocket::bind(server_addr()).expect("6466 already taken — is a dev server running?");
        let dir = scratch("port");
        let marker = dir.join("spawned");
        let bin = script(&dir, &format!("touch {}", marker.display()));
        let err = LocalHost::start_with(&bin, Path::new("t.world"), &dir.join("server.log")).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AddrInUse);
        assert_eq!(err.to_string(), PORT_IN_USE);
        std::thread::sleep(Duration::from_millis(100));
        assert!(!marker.exists(), "spawned despite the taken port");
    }
}
