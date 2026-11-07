//! The `login` binary's admin subcommands ([`run_ctl`]): signing keys, the world registry and
//! account fixes. `serve` is the binary's own (it needs a long-running runtime).
//!
//! Global flag `--db <url>` overrides `VOXEL_LOGIN_DB`; `keygen --dir <d>` overrides
//! `VOXEL_LOGIN_KEYS_DIR`.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use login_api::WorldStatus;

use crate::{Hasher, LoginConfig, LoginStore, WorldRow, check_password, normalize_email};

/// Digits in a key file name (`0001.pem`); [`auth::Signer::load_dir`] sorts byte-wise.
pub const KEY_FILE_DIGITS: usize = 4;
/// Mode of a key file.
pub const KEY_FILE_MODE: u32 = 0o600;
/// Mode of a created keys directory.
pub const KEY_DIR_MODE: u32 = 0o700;

/// What [`run_ctl_with`] needs from outside, so tests can inject cheap hashing and a password.
pub struct CtlDeps<'a> {
    pub hasher: Hasher,
    /// Where `account set-password` reads the password (one line).
    pub stdin: &'a mut dyn BufRead,
}

const USAGE: &str = "usage: login [--db <url>] <command>
  serve
  keygen [--dir <d>]
  world add <id> --name <n> [--quic host:port] [--wt url] [--cert-hash h] [--closed]
  world list | remove <id> | open <id> | close <id>
  account set-password <email>   (password on stdin)
  account verify <email>";

/// Run one admin command with the real hasher and stdin.
pub fn run_ctl(args: &[String], out: &mut dyn Write) -> Result<(), String> {
    let stdin = std::io::stdin();
    let mut lock = stdin.lock();
    run_ctl_with(
        args,
        out,
        CtlDeps {
            hasher: Hasher::default(),
            stdin: &mut lock,
        },
    )
}

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

/// [`run_ctl`] with injected dependencies.
pub fn run_ctl_with(args: &[String], out: &mut dyn Write, deps: CtlDeps<'_>) -> Result<(), String> {
    let mut args = args.to_vec();
    let db = take_opt(&mut args, "--db")?;
    let dir = take_opt(&mut args, "--dir")?;
    let name = take_opt(&mut args, "--name")?;
    let quic = take_opt(&mut args, "--quic")?;
    let wt = take_opt(&mut args, "--wt")?;
    let cert_hash = take_opt(&mut args, "--cert-hash")?;
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
            let row = WorldRow {
                world_id: id.to_string(),
                name,
                status: if closed { WorldStatus::Closed } else { WorldStatus::Open },
                quic,
                webtransport: wt,
                cert_hash,
                created_at: now_secs(),
            };
            block_on(async {
                let store = open(&db_url()?).await?;
                store.put_world(&row).await.map_err(|e| e.to_string())
            })?;
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
                    "{}\t{status}\t{}\tquic={}\twt={}\tcert_hash={}",
                    w.world_id,
                    w.name,
                    w.quic.as_deref().unwrap_or("-"),
                    w.webtransport.as_deref().unwrap_or("-"),
                    w.cert_hash.as_deref().unwrap_or("-"),
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
        ["account", "set-password", email] => {
            let email = normalize_email(email).map_err(|e| e.to_string())?;
            let mut line = String::new();
            deps.stdin.read_line(&mut line).map_err(io)?;
            let password = line.trim_end_matches(['\n', '\r']);
            check_password(password).map_err(|e| e.to_string())?;
            let hash = deps.hasher.hash(password);
            block_on(async {
                let store = open(&db_url()?).await?;
                let acc = store
                    .account_by_email(&email)
                    .await
                    .map_err(|e| e.to_string())?
                    .ok_or_else(|| format!("no account {email:?}"))?;
                store.set_password(acc.account_id, &hash).await.map_err(|e| e.to_string())?;
                // An admin reset logs old sessions out.
                store.revoke_account_refresh(acc.account_id, now_secs()).await.map_err(|e| e.to_string())
            })?;
            writeln!(out, "password set for {email}; refresh tokens revoked").map_err(io)
        }
        ["account", "verify", email] => {
            let email = normalize_email(email).map_err(|e| e.to_string())?;
            block_on(async {
                let store = open(&db_url()?).await?;
                let acc = store
                    .account_by_email(&email)
                    .await
                    .map_err(|e| e.to_string())?
                    .ok_or_else(|| format!("no account {email:?}"))?;
                store.mark_verified(acc.account_id, now_secs()).await.map_err(|e| e.to_string())
            })?;
            writeln!(out, "{email} verified").map_err(io)
        }
        ["serve"] => Err("`serve` is run by the binary itself".into()),
        _ => Err(USAGE.into()),
    }
}

async fn open(url: &str) -> Result<LoginStore, String> {
    LoginStore::open(url).await.map_err(|e| format!("login database {url}: {e}"))
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
