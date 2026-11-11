//! The local server a menu-started world session plays on: the `server`
//! binary as a child process, serving exactly one world file (one world per
//! server process — `forged::install` is a `OnceLock`). Native only.
//!
//! The child is the binary next to the client executable, or
//! `VOXEL_SERVER_BIN`. It runs with `VOXEL_WORLD_FILE=<terrain>` and
//! `VOXEL_SINGLE_PLAYER=1` (it exits once its last client leaves) and writes
//! stdout + stderr to the world's server log. It binds a free UDP port the
//! client picks ([`free_udp_port`], passed as `VOXEL_SERVER_PORT`), so a menu
//! world runs alongside a dev server on 6466; a single-player server starts
//! no WebTransport ingress. If another process grabs the port between the
//! pick and the child's bind, the child exits and the session's exit watch
//! reports it with the server log's tail.
//!
//! The child's stdin is a pipe whose write end only the client holds: a
//! single-player server exits when it closes, so the OS stops the child
//! however the client dies — even before the server ever saw a connection.

use std::ffi::OsString;
use std::fs::File;
use std::io;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::path::{Path, PathBuf};
use std::io::{Read, Seek, SeekFrom};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use bevy::prelude::Resource;

/// How long [`LocalHost::stop`] waits for the server to exit on its own (it
/// does once its last client has left) before killing it. Covers the
/// server's orderly exit: parking every region (`server::cuts::EXIT_PARK_TIMEOUT`,
/// 10 s at most) and committing the last save cut — normally well under a
/// second each. A kill mid-commit is safe (the commit is one transaction):
/// the world restarts from its previous cut.
pub const STOP_GRACE: Duration = Duration::from_secs(30);

/// How often [`LocalHost::stop`] polls the child while it waits.
const STOP_POLL: Duration = Duration::from_millis(20);

/// Overrides the server binary [`server_binary`] finds.
pub const SERVER_BIN_ENV: &str = "VOXEL_SERVER_BIN";

/// The server's single-player switch (`server::SINGLE_PLAYER_ENV`; the client
/// does not depend on the server crate).
pub const SINGLE_PLAYER_ENV: &str = "VOXEL_SINGLE_PLAYER";

/// The server's QUIC port override (`server::SERVER_PORT_ENV`).
pub const SERVER_PORT_ENV: &str = "VOXEL_SERVER_PORT";

/// How much of the server log's end [`LocalHost::log_tail`] reads.
const LOG_TAIL_BYTES: u64 = 64 * 1024;

/// A running child server.
#[derive(Resource, Debug)]
pub struct LocalHost {
    child: Child,
    /// The write end of the child's stdin; closing it tells a single-player
    /// server its parent is gone.
    stdin: Option<ChildStdin>,
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
    // The stud world's `terrain` is the keyword, not a file: its log lives in the entry dir too.
    if terrain.file_name().is_some_and(|n| n == worldgen::world_store::TERRAIN_FILE) || terrain == Path::new("studs") || terrain == Path::new("procedural") || terrain == Path::new("prefab-editor") {
        entry_dir.join("server.log")
    } else {
        terrain.with_extension("server.log")
    }
}

/// The last `lines` lines within the last `max_bytes` of the file at `path`.
fn tail_lines(path: &Path, lines: usize, max_bytes: u64) -> String {
    let Ok(mut file) = File::open(path) else { return String::new() };
    let len = file.metadata().map_or(0, |m| m.len());
    let start = len.saturating_sub(max_bytes);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return String::new();
    }
    let mut bytes = Vec::new();
    if file.read_to_end(&mut bytes).is_err() {
        return String::new();
    }
    let text = String::from_utf8_lossy(&bytes);
    let mut all: Vec<&str> = text.lines().collect();
    if start > 0 && !all.is_empty() {
        all.remove(0); // a partial line cut by the seek
    }
    all[all.len().saturating_sub(lines)..].join("\n")
}

/// A UDP port on 127.0.0.1 that is free right now: bind port 0, read what the
/// OS assigned, release it. QUIC is UDP, so this is the port the child binds.
pub fn free_udp_port() -> io::Result<u16> {
    let socket = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))?;
    Ok(socket.local_addr()?.port())
}

impl LocalHost {
    /// Start the server binary ([`server_binary`]) on `terrain`, logging to `log`,
    /// on `port` (`None` = a free one, [`free_udp_port`]).
    pub fn start(terrain: &Path, log: &Path, port: Option<u16>) -> io::Result<LocalHost> {
        Self::start_on(&server_binary(), terrain, log, port)
    }

    /// [`start`](Self::start) with an explicit binary, on a free port.
    pub fn start_with(binary: &Path, terrain: &Path, log: &Path) -> io::Result<LocalHost> {
        Self::start_on(binary, terrain, log, None)
    }

    /// [`start`](Self::start) with an explicit binary and port.
    pub fn start_on(binary: &Path, terrain: &Path, log: &Path, port: Option<u16>) -> io::Result<LocalHost> {
        let port = match port {
            Some(port) => port,
            None => free_udp_port()?,
        };
        if let Some(dir) = log.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let out = File::create(log)?;
        let err = out.try_clone()?;
        let mut child = Command::new(binary)
            .env("VOXEL_WORLD_FILE", terrain)
            .env("VOXEL_WORLD_DB", worldgen::world_store::store_path_for(terrain))
            .env(SINGLE_PLAYER_ENV, "1")
            .env(SERVER_PORT_ENV, port.to_string())
            .stdin(Stdio::piped())
            .stdout(out)
            .stderr(err)
            .spawn()
            .map_err(|e| io::Error::new(e.kind(), format!("could not start {}: {e}", binary.display())))?;
        let stdin = child.stdin.take();
        Ok(LocalHost { child, stdin, log: log.to_path_buf(), addr: SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port)) })
    }

    /// Close the child's stdin: a single-player server takes it as its
    /// parent being gone and exits.
    pub fn close_stdin(&mut self) {
        self.stdin = None;
    }

    /// The child's exit status, if it has exited.
    pub fn try_exited(&mut self) -> Option<ExitStatus> {
        self.child.try_wait().ok().flatten()
    }

    /// Wait up to `grace` for the server to exit by itself, then kill it.
    /// Returns its exit status (`None` if it could not be reaped).
    pub fn stop(mut self, grace: Duration) -> Option<ExitStatus> {
        self.close_stdin();
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
    /// Reads only the log's last [`LOG_TAIL_BYTES`].
    pub fn log_tail(&self, lines: usize) -> String {
        tail_lines(&self.log, lines, LOG_TAIL_BYTES)
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
        let dir = scratch("env");
        let bin = script(&dir, "env; echo done");
        let log = dir.join("logs/server.log");
        let mut host = LocalHost::start_with(&bin, Path::new("/worlds/x/terrain.world"), &log).unwrap();
        assert_eq!(host.addr.ip(), std::net::IpAddr::from(Ipv4Addr::LOCALHOST));
        wait_for(|| host.try_exited().is_some());
        let text = std::fs::read_to_string(&log).unwrap();
        assert!(text.contains("VOXEL_WORLD_FILE=/worlds/x/terrain.world"), "{text}");
        assert!(text.contains("VOXEL_SINGLE_PLAYER=1"), "{text}");
        assert_eq!(host.log_tail(1), "done");
    }

    #[test]
    #[ignore = "slow: >100ms — run via just test-full"]
    fn stop_kills_after_grace() {
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
    fn closing_stdin_ends_the_child() {
        let dir = scratch("stdin");
        let bin = script(&dir, "cat >/dev/null; echo gone");
        let mut host = LocalHost::start_with(&bin, Path::new("t.world"), &dir.join("server.log")).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        assert!(host.try_exited().is_none(), "exited while its stdin was open");
        host.close_stdin();
        wait_for(|| host.try_exited().is_some());
        assert_eq!(host.log_tail(1), "gone");
    }

    #[test]
    fn log_tail_reads_only_the_end() {
        let dir = scratch("tail");
        let log = dir.join("server.log");
        let body: String = (0..2000).map(|i| format!("line {i}\n")).collect();
        std::fs::write(&log, body).unwrap();
        assert_eq!(tail_lines(&log, 2, 64 * 1024), "line 1998\nline 1999");
        // A 30-byte window starts mid-line: the cut line is dropped.
        let tail = tail_lines(&log, 100, 30);
        assert!(tail.lines().all(|l| l.starts_with("line ")), "{tail}");
        assert!(tail.ends_with("line 1999"), "{tail}");
        assert_eq!(tail_lines(&dir.join("missing.log"), 3, 1024), "");
    }

    #[test]
    #[ignore = "slow: >100ms — run via just test-full"]
    fn start_passes_a_free_port_to_the_child() {
        let dir = scratch("port");
        let bin = script(&dir, "echo \"port=$VOXEL_SERVER_PORT\"");
        let log = dir.join("server.log");
        let mut host = LocalHost::start_with(&bin, Path::new("t.world"), &log).unwrap();
        wait_for(|| host.try_exited().is_some());
        let port = host.addr.port();
        assert_ne!(port, 0);
        assert_ne!(port, 6466, "a menu world must not take the dev server's port");
        assert_eq!(host.addr.ip(), std::net::IpAddr::from(Ipv4Addr::LOCALHOST));
        assert_eq!(host.log_tail(1), format!("port={port}"));
    }

    #[test]
    #[ignore = "slow: >100ms — run via just test-full"]
    fn start_works_while_6466_is_taken() {
        // Holding 6466 may fail if a dev server already has it — either way
        // something holds it, which is the case under test.
        let _taken = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 6466));
        let dir = scratch("taken");
        let bin = script(&dir, "echo started");
        let mut host = LocalHost::start_with(&bin, Path::new("t.world"), &dir.join("server.log")).unwrap();
        wait_for(|| host.try_exited().is_some());
        assert_eq!(host.log_tail(1), "started");
    }
}
