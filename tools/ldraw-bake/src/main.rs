//! `ldraw-bake [--ldraw DIR] [--check]`: bake the parts in `parts.txt` into the committed stud assets.
//! Run from the repository root (`just ldraw-bake`).

mod classify;
mod emit;
mod flatten;

use anyhow::{Context, Result, bail};
use std::path::PathBuf;

const PARTS_TXT: &str = include_str!("../parts.txt");

fn main() -> Result<()> {
    let mut ldraw = PathBuf::from(std::env::var("LDRAW_DIR").unwrap_or_else(|_| "assets/ldraw".into()));
    let mut check = false;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--ldraw" => ldraw = PathBuf::from(args.next().context("--ldraw needs a directory")?),
            "--check" => check = true,
            other => bail!("unknown argument `{other}` (usage: ldraw-bake [--ldraw DIR] [--check])"),
        }
    }
    let lib = flatten::Library::new(&ldraw);
    let mut parts = Vec::new();
    for number in PARTS_TXT.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        let p = classify::bake_part(&lib, number).with_context(|| format!("baking {number}"))?;
        eprintln!(
            "{number}: {} size {:?} studs {} hull {} tris hi/lo {}/{}",
            p.def.name,
            p.def.size,
            p.def.studs.len(),
            p.def.hull.len(),
            p.hi.tris.len(),
            p.lo.tris.len()
        );
        parts.push(p);
    }
    let outs = emit::outputs(&lib, &parts)?;
    let stale = emit::write_or_check(&PathBuf::from("."), &outs, check)?;
    if check {
        if stale.is_empty() {
            eprintln!("ldraw-bake: outputs up to date");
        } else {
            for p in &stale {
                eprintln!("ldraw-bake: out of date: {}", p.display());
            }
            std::process::exit(1);
        }
    } else {
        eprintln!("ldraw-bake: wrote {} file(s), {} unchanged", stale.len(), outs.len() - stale.len());
    }
    Ok(())
}
