//! The `login` binary's admin subcommands ([`run_ctl`]): signing keys, the world registry and
//! account fixes. `serve` is the binary's own (it needs a long-running runtime).
//!
//! Global flag `--db <url>` overrides `VOXEL_LOGIN_DB`; `keygen --dir <d>` overrides
//! `VOXEL_LOGIN_KEYS_DIR`.

use std::io::Write;
use std::path::{Path, PathBuf};

use login_api::WorldStatus;

use crate::{LoginConfig, LoginStore, WorldRow, normalize_email};

/// Digits in a key file name (`0001.pem`); [`auth::Signer::load_dir`] sorts byte-wise.
pub const KEY_FILE_DIGITS: usize = 4;
/// Mode of a key file.
pub const KEY_FILE_MODE: u32 = 0o600;
/// Mode of a created keys directory.
pub const KEY_DIR_MODE: u32 = 0o700;

const USAGE: &str = "usage: login [--db <url>] <command>
  serve
  keygen [--dir <d>]
  world add <id> --name <n> [--quic host:port] [--wt url] [--cert-hash h] [--closed]
            [--link host:port --link-cert <sha256>]   (the host's service link: join pushes SpawnPlayer)
  world list | remove <id> | open <id> | close <id>
  account issue-refresh <email>   (prints a new refresh token; creates the account, verified,
                                   if missing; an existing account is left as it is)
  account revoke <email>   (revokes its refresh tokens and kicks its open sessions)";

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Removes `--flag value` from `args`, returning the value.
fn take_opt(args: &mut Vec<String>, flag: &str) -> Result<Option<String>, String> {
    let Some(i) = args.iter().position(|a| a == flag) else {
        return Ok(None);
    };
    if i + 1 >= args.len() {
        return Err(format!("{flag} needs a value"));
    }
    let v = args.remove(i + 1);
    args.remove(i);
    Ok(Some(v))
}

fn take_flag(args: &mut Vec<String>, flag: &str) -> bool {
    match args.iter().position(|a| a == flag) {
        Some(i) => {
            args.remove(i);
            true
        }
        None => false,
    }
}

/// How the binary routes its command line: the global `--db <url>` (if any) and whether the
/// command is `serve` (also when there is none).
#[derive(Debug, PartialEq, Eq)]
pub struct Route {
    pub db: Option<String>,
    pub serve: bool,
}

/// Routes `args`; `--db <url>` is stripped first, wherever it stands.
pub fn route(args: &[String]) -> Result<Route, String> {
    let mut rest = args.to_vec();
    let db = take_opt(&mut rest, "--db")?;
    let serve = rest.first().is_none_or(|c| c == "serve");
    Ok(Route { db, serve })
}

/// Run one admin command, writing its output to `out`.
pub fn run_ctl(args: &[String], out: &mut dyn Write) -> Result<(), String> {
    let mut args = args.to_vec();
    let db = take_opt(&mut args, "--db")?;
    let dir = take_opt(&mut args, "--dir")?;
    let name = take_opt(&mut args, "--name")?;
    let quic = take_opt(&mut args, "--quic")?;
    let wt = take_opt(&mut args, "--wt")?;
    let cert_hash = take_opt(&mut args, "--cert-hash")?;
    let link = take_opt(&mut args, "--link")?;
    let link_cert = take_opt(&mut args, "--link-cert")?;
    let closed = take_flag(&mut args, "--closed");
    let words: Vec<&str> = args.iter().map(String::as_str).collect();

    let config = || LoginConfig::from_env();
    let db_url = || -> Result<String, String> { Ok(db.clone().unwrap_or(config()?.db_url)) };
    let io = |e: std::io::Error| e.to_string();

    match words.as_slice() {
        ["keygen"] => {
            let dir = match dir {
                Some(d) => PathBuf::from(d),
                None => config()?.keys_dir,
            };
            let kid = keygen(&dir)?;
            writeln!(out, "{kid}").map_err(io)
        }
        ["world", "add", id] => {
            let name = name.ok_or("world add needs --name")?;
            let (host_link, host_link_cert) = host_link_args(link, link_cert)?;
            let row = WorldRow {
                world_id: id.to_string(),
                name,
                status: if closed { WorldStatus::Closed } else { WorldStatus::Open },
                quic,
                webtransport: wt,
                cert_hash,
                created_at: now_secs(),
                host_link,
                host_link_cert,
            };
            let cleared = block_on(async {
                let store = open(&db_url()?).await?;
                // `world add` replaces the row: a world that had a host link loses it unless
                // `--link` is given again (joins stop pushing, its kicks wait) — said aloud.
                let before = store.world(&row.world_id).await.map_err(|e| e.to_string())?;
                store.put_world(&row).await.map_err(|e| e.to_string())?;
                Ok::<_, String>(before.and_then(|w| w.host_link).filter(|_| row.host_link.is_none()))
            })?;
            if let Some(old) = cleared {
                writeln!(
                    out,
                    "warning: world {id} had host link {old}; without --link it is cleared (joins no longer \
                     push SpawnPlayer, kicks wait) — run `world add` again with --link/--link-cert to keep it"
                )
                .map_err(io)?;
            }
            writeln!(out, "world {id} added").map_err(io)
        }
        ["world", "list"] => {
            let worlds = block_on(async { open(&db_url()?).await?.worlds().await.map_err(|e| e.to_string()) })?;
            for w in worlds {
                let status = match w.status {
                    WorldStatus::Open => "open",
                    WorldStatus::Closed => "closed",
                };
                writeln!(
                    out,
                    "{}\t{status}\t{}\tquic={}\twt={}\tcert_hash={}\tlink={}",
                    w.world_id,
                    w.name,
                    w.quic.as_deref().unwrap_or("-"),
                    w.webtransport.as_deref().unwrap_or("-"),
                    w.cert_hash.as_deref().unwrap_or("-"),
                    w.host_link.as_deref().unwrap_or("-"),
                )
                .map_err(io)?;
            }
            Ok(())
        }
        ["world", "remove", id] => {
            let removed = block_on(async { open(&db_url()?).await?.remove_world(id).await.map_err(|e| e.to_string()) })?;
            if !removed {
                return Err(format!("no world {id:?}"));
            }
            writeln!(out, "world {id} removed").map_err(io)
        }
        ["world", cmd @ ("open" | "close"), id] => {
            let status = if *cmd == "open" { WorldStatus::Open } else { WorldStatus::Closed };
            block_on(async {
                let store = open(&db_url()?).await?;
                let mut w = store
                    .world(id)
                    .await
                    .map_err(|e| e.to_string())?
                    .ok_or_else(|| format!("no world {id:?}"))?;
                w.status = status;
                store.put_world(&w).await.map_err(|e| e.to_string())
            })?;
            writeln!(out, "world {id} {cmd}ed").map_err(io)
        }
        ["account", "issue-refresh", email] => {
            // For launches with no screens: the token is the only thing printed (the recipe
            // redirects it into the client's token file). An account made here has no password
            // (Keycloak owns those) and is verified; an existing one is left as it is (never
            // marked verified: that would launder an unverified squatter's account).
            let email = normalize_email(email).map_err(|e| e.to_string())?;
            let token = block_on(async {
                let store = open(&db_url()?).await?;
                let err = |e: crate::LoginError| e.to_string();
                let now = now_secs();
                let id = store.ensure_verified_account(&email, now).await.map_err(err)?;
                crate::api::mint_refresh(&store, id, now).await.map_err(err)
            })?;
            writeln!(out, "{token}").map_err(io)
        }
        ["account", "revoke", email] => {
            // Ends every session of the account: its refresh tokens are revoked and a kick is
            // queued for each open session (the running login service's sweeper sends them).
            let email = normalize_email(email).map_err(|e| e.to_string())?;
            let kicks = block_on(async {
                let store = open(&db_url()?).await?;
                let acc = store
                    .account_by_email(&email)
                    .await
                    .map_err(|e| e.to_string())?
                    .ok_or_else(|| format!("no account {email:?}"))?;
                store
                    .revoke_account_with_kicks(acc.account_id, now_secs(), crate::api::KICK_ACCOUNT_REVOKED)
                    .await
                    .map_err(|e| e.to_string())
            })?;
            writeln!(out, "{email}: refresh tokens revoked, {kicks} session(s) kicked").map_err(io)
        }
        ["serve"] => Err("`serve` is run by the binary itself".into()),
        _ => Err(USAGE.into()),
    }
}

/// `--link host:port` and `--link-cert <sha256>` go together; the address must parse and the
/// pin be 64 hex digits (stored lowercase).
fn host_link_args(link: Option<String>, cert: Option<String>) -> Result<(Option<String>, Option<String>), String> {
    match (link, cert) {
        (None, None) => Ok((None, None)),
        (Some(link), Some(cert)) => {
            svc_link::LinkTarget::parse(&link).map_err(|e| format!("--link {link:?}: {e} (expected host:port)"))?;
            let cert = cert.trim().to_ascii_lowercase();
            if cert.len() != 64 || !cert.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err("--link-cert is the host link's SHA-256 pin: 64 hex digits".into());
            }
            Ok((Some(link), Some(cert)))
        }
        _ => Err("--link and --link-cert go together".into()),
    }
}

async fn open(url: &str) -> Result<LoginStore, String> {
    LoginStore::open(url).await.map_err(|e| format!("login database {}: {e}", crate::config::redact_url(url)))
}

fn block_on<T>(f: impl std::future::Future<Output = Result<T, String>>) -> Result<T, String> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?
        .block_on(f)
}

/// Writes the next `NNNN.pem` into `dir` (created 0700 if missing) and returns its `kid`.
fn keygen(dir: &Path) -> Result<String, String> {
    use std::io::Write as _;
    #[cfg(unix)]
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};

    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    builder.mode(KEY_DIR_MODE);
    builder.create(dir).map_err(|e| format!("{}: {e}", dir.display()))?;

    let mut next = 1u32;
    for entry in std::fs::read_dir(dir).map_err(|e| e.to_string())? {
        let path = entry.map_err(|e| e.to_string())?.path();
        if path.extension().is_some_and(|e| e == auth::KEY_FILE_EXTENSION)
            && let Some(n) = path.file_stem().and_then(|s| s.to_str()).and_then(|s| s.parse::<u32>().ok())
        {
            next = next.max(n + 1);
        }
    }
    let (key, pem) = auth::SigningKey::generate();
    let path = dir.join(format!("{next:0width$}.{}", auth::KEY_FILE_EXTENSION, width = KEY_FILE_DIGITS));
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    opts.mode(KEY_FILE_MODE);
    let mut f = opts.open(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    f.write_all(pem.as_bytes()).map_err(|e| e.to_string())?;
    Ok(key.kid().to_string())
}

