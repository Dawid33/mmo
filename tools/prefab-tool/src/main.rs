//! `prefab-tool view <name-or-path> [-o <png>] [--scale <px per block>]`, `prefab-tool check [<dir>]` and
//! `prefab-tool town <style> [flags]` (a top-down picture of a town layout).
//! Run from the repo root (the recipes in the justfile do).

mod render;
mod town;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use game::prefab::{
    load_builtin_prefabs, prefab_from_named_bytes, resolve, ResolvedPrefab, Trust, BUILTIN_PREFAB_DIR, BIN_SUFFIX,
    RON_SUFFIX,
};
use game::registry::BlockRegistry;

pub(crate) const BLOCKS_DIR: &str = "assets/blocks";
pub(crate) const USAGE: &str = "usage:\n  prefab-tool view <name-or-path> [-o <png>] [--scale <px per block>]\n  prefab-tool check [<dir>]\n  prefab-tool town <style> [--dirs <a:b:..>] [--seed N] [--arms 1-4] [--len 16-256] [--terrain flat|seed] [-o <png>] [--scale N]";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("view") => view(&args[1..]),
        Some("check") => check(&args[1..]),
        Some("town") => town::run(&args[1..]),
        _ => Err((2, USAGE.to_string())),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err((code, message)) => {
            eprintln!("{message}");
            ExitCode::from(code)
        }
    }
}

pub(crate) type Failure = (u8, String);

pub(crate) fn load_registry() -> Result<BlockRegistry, Failure> {
    let path = Path::new(BLOCKS_DIR).join("blocks.ron");
    let src = std::fs::read_to_string(&path).map_err(|e| {
        (2, format!("cannot read {}: {e} (run prefab-tool from the repository root)", path.display()))
    })?;
    BlockRegistry::from_ron(&src).map_err(|e| (2, format!("{}: {e}", path.display())))
}

fn check(args: &[String]) -> Result<(), Failure> {
    if args.len() > 1 {
        return Err((2, USAGE.to_string()));
    }
    let registry = load_registry()?;
    let dir = args.first().map(String::as_str).unwrap_or(BUILTIN_PREFAB_DIR);
    if !Path::new(dir).is_dir() {
        return Err((2, format!("{dir}: not a directory")));
    }
    let set = load_builtin_prefabs(Some(Path::new(dir)), &|n| registry.id_of(n)).map_err(|e| (1, e))?;
    let mut prefabs: Vec<&ResolvedPrefab> = set.names().filter_map(|n| set.named(n).map(|p| &**p)).collect();
    prefabs.sort_by(|a, b| a.name.cmp(&b.name));
    for p in &prefabs {
        println!(
            "{}  {}x{}x{}  {} blocks  {}",
            p.name,
            p.size[0],
            p.size[1],
            p.size[2],
            p.solid_count(),
            p.tags.join(",")
        );
    }
    println!("{} prefabs ok", prefabs.len());
    Ok(())
}

struct ViewArgs {
    target: String,
    out: Option<PathBuf>,
    scale: Option<u32>,
}

fn parse_view(args: &[String]) -> Result<ViewArgs, Failure> {
    let (mut target, mut out, mut scale) = (None, None, None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-o" => out = Some(PathBuf::from(it.next().ok_or((2, "-o needs a path".to_string()))?)),
            "--scale" => {
                let v = it.next().ok_or((2, "--scale needs a number".to_string()))?;
                match v.parse::<u32>() {
                    Ok(n) if n >= 1 => scale = Some(n),
                    _ => return Err((2, format!("--scale: \"{v}\" is not a whole number of at least 1"))),
                }
            }
            flag if flag.starts_with('-') => return Err((2, format!("unknown flag {flag}\n{USAGE}"))),
            _ if target.is_none() => target = Some(a.clone()),
            _ => return Err((2, USAGE.to_string())),
        }
    }
    let target = target.ok_or((2, USAGE.to_string()))?;
    Ok(ViewArgs { target, out, scale })
}

/// A path as given, else `assets/prefabs/<name>.prefab.ron|.prefab.bin`.
fn find_prefab_file(target: &str) -> Result<PathBuf, Failure> {
    let direct = Path::new(target);
    if direct.is_file() {
        return Ok(direct.to_path_buf());
    }
    for suffix in [RON_SUFFIX, BIN_SUFFIX] {
        let p = Path::new(BUILTIN_PREFAB_DIR).join(format!("{target}{suffix}"));
        if p.is_file() {
            return Ok(p);
        }
    }
    Err((2, format!("no prefab \"{target}\": not a file, and no {BUILTIN_PREFAB_DIR}/{target}{RON_SUFFIX}")))
}

fn view(args: &[String]) -> Result<(), Failure> {
    let a = parse_view(args)?;
    let registry = load_registry()?;
    let file_path = find_prefab_file(&a.target)?;
    let file_name = file_path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let bytes = std::fs::read(&file_path).map_err(|e| (2, format!("{}: {e}", file_path.display())))?;
    let file = prefab_from_named_bytes(&file_name, &bytes, None)
        .ok_or((1, format!("{file_name}: not a prefab file name")))?
        .map_err(|e| (1, format!("{file_name}: {e}")))?;
    let prefab = resolve(&file, &|n| registry.id_of(n), Trust::Builtin).map_err(|problems| {
        let all: Vec<String> = problems.iter().map(|p| p.to_string()).collect();
        (1, format!("{file_name}: {} problem(s): {}", all.len(), all.join("; ")))
    })?;

    let colours = render::BlockColours::load(&registry, Path::new(BLOCKS_DIR));
    let scale = a.scale.unwrap_or_else(|| render::default_scale(prefab.size));
    let image = render::render(&prefab, &colours, scale);
    let out = a.out.unwrap_or_else(|| Path::new("target/prefab-view").join(format!("{}.png", prefab.name)));
    if let Some(dir) = out.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| (2, format!("{}: {e}", dir.display())))?;
    }
    render::write_png(&out, &image).map_err(|e| (2, format!("{}: {e}", out.display())))?;

    println!("{}", out.display());
    println!("size {}x{}x{} blocks, {} solid, scale {scale}", prefab.size[0], prefab.size[1], prefab.size[2], prefab.solid_count());
    println!("tags: {}", if prefab.tags.is_empty() { "-".to_string() } else { prefab.tags.join(", ") });
    let mut kinds: BTreeMap<&str, usize> = BTreeMap::new();
    for m in &prefab.markers {
        *kinds.entry(m.kind.as_str()).or_default() += 1;
    }
    let kinds: Vec<String> = kinds.iter().map(|(k, n)| format!("{k} x{n}")).collect();
    println!("markers: {}", if kinds.is_empty() { "-".to_string() } else { kinds.join(", ") });
    Ok(())
}
