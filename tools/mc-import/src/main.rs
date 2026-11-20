//! `mc-import report|import|split <file.schem>…`: turns Sponge schematics into prefab files.
//! Run from the repo root (the `just mc-import` recipe does): it needs `assets/blocks/blocks.ron`.
//! Output goes to `prefab-imports/<file stem>/` by default, never under `assets/`.

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};
use std::process::ExitCode;

use game::prefab::{BIN_SUFFIX, Trust, prefab_to_bin, resolve};
use game::registry::BlockRegistry;
use mc_import::{
    AirMode, BlockVolume, Build, ConvertOptions, GroundMode, Report, SplitOptions, Table, convert,
    read_schem, slug, split,
};

const BLOCKS_RON: &str = "assets/blocks/blocks.ron";
const DEFAULT_OUT: &str = "prefab-imports";
const REPORT_SUFFIX: &str = ".report.txt";
const TOP_UNMAPPED: usize = 10;
const USAGE: &str = "usage:
  mc-import report <file.schem>...
  mc-import import <file.schem> [--name N] [--out DIR] [--air auto|carve|keep] [--licence TEXT] [--author TEXT] [--tag T]...
  mc-import split <file.schem> [--prefix P] [--out DIR] [--gap N] [--min-blocks N] [--ground auto|none|N]
                  [--labels] [--air auto|carve|keep] [--licence TEXT] [--author TEXT] [--tag T]...

split is for showroom files (many separate builds on a plate). A single composition (a castle, a
stadium) should be imported instead: the build count `report` prints is the cue. Builds are numbered
<prefix>-<nn>; --labels names them after a nearby sign instead (signs inside a build are ignored).
A prefab can be promoted into assets/prefabs/ only if it was imported with --licence.
File stems repeat across packs: set --name (import) or --prefix (split) for each pack.";

type Failure = (u8, String);

fn main() -> ExitCode {
    let mut args: Vec<String> = Vec::new();
    for a in std::env::args_os().skip(1) {
        match a.into_string() {
            Ok(a) => args.push(a),
            Err(a) => {
                eprintln!("argument {a:?} is not valid UTF-8\n{USAGE}");
                return ExitCode::from(2);
            }
        }
    }
    match run(&args) {
        Ok(0) => ExitCode::SUCCESS,
        Ok(code) => ExitCode::from(code),
        Err((code, message)) => {
            eprintln!("{message}");
            ExitCode::from(code)
        }
    }
}

/// What the command line asked for (the flags of every subcommand; each rejects the others').
struct Opts {
    files: Vec<String>,
    name: Option<String>,
    out: Option<String>,
    gap: Option<u32>,
    min_blocks: Option<u64>,
    ground: Option<GroundMode>,
    air: AirMode,
    labels: bool,
    licence: String,
    author: Option<String>,
    tags: Vec<String>,
}

fn parse(cmd: &str, args: &[String]) -> Result<Opts, Failure> {
    let usage = |m: String| -> Failure { (2, format!("{m}\n{USAGE}")) };
    let mut o = Opts {
        files: Vec::new(),
        name: None,
        out: None,
        gap: None,
        min_blocks: None,
        ground: None,
        air: AirMode::Auto,
        labels: false,
        licence: String::new(),
        author: None,
        tags: Vec::new(),
    };
    let name_flag = if cmd == "split" { "--prefix" } else { "--name" };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = |flag: &str| {
            it.next()
                .cloned()
                .ok_or_else(|| usage(format!("{flag} needs a value")))
        };
        match a.as_str() {
            f if f == name_flag && cmd != "report" => o.name = Some(value(f)?),
            "--out" if cmd != "report" => o.out = Some(value("--out")?),
            "--air" if cmd != "report" => {
                o.air = match value("--air")?.as_str() {
                    "auto" => AirMode::Auto,
                    "carve" => AirMode::Carve,
                    "keep" => AirMode::Keep,
                    v => return Err(usage(format!("--air: \"{v}\" is not auto, carve or keep"))),
                }
            }
            "--licence" if cmd != "report" => o.licence = value("--licence")?,
            "--author" if cmd != "report" => o.author = Some(value("--author")?),
            "--tag" if cmd != "report" => o.tags.push(value("--tag")?),
            "--labels" if cmd == "split" => o.labels = true,
            "--gap" if cmd == "split" => {
                let v = value("--gap")?;
                o.gap = Some(
                    v.parse()
                        .map_err(|_| usage(format!("--gap: \"{v}\" is not a whole number")))?,
                );
            }
            "--min-blocks" if cmd == "split" => {
                let v = value("--min-blocks")?;
                o.min_blocks =
                    Some(v.parse().map_err(|_| {
                        usage(format!("--min-blocks: \"{v}\" is not a whole number"))
                    })?);
            }
            "--ground" if cmd == "split" => {
                o.ground = Some(match value("--ground")?.as_str() {
                    "auto" => GroundMode::Auto,
                    "none" => GroundMode::None,
                    v => GroundMode::Layers(v.parse().map_err(|_| {
                        usage(format!("--ground: \"{v}\" is not auto, none or a number"))
                    })?),
                });
            }
            flag if flag.starts_with('-') => return Err(usage(format!("unknown flag {flag}"))),
            _ => o.files.push(a.clone()),
        }
    }
    if o.files.is_empty() || (cmd != "report" && o.files.len() != 1) {
        return Err((2, USAGE.to_string()));
    }
    Ok(o)
}

fn run(args: &[String]) -> Result<u8, Failure> {
    let cmd = match args.first().map(String::as_str) {
        Some(c @ ("report" | "import" | "split")) => c,
        _ => return Err((2, USAGE.to_string())),
    };
    let opts = parse(cmd, &args[1..])?;
    let registry = load_registry()?;
    let mut failed = false;
    for file in &opts.files {
        let result = match cmd {
            "report" => report(file),
            _ => write_prefabs(cmd == "split", file, &opts, &registry),
        };
        match result {
            Ok(ok) => failed |= !ok,
            Err((2, m)) => return Err((2, m)),
            Err((_, m)) => {
                eprintln!("{file}: {m}");
                failed = true;
            }
        }
    }
    Ok(u8::from(failed))
}

fn load_registry() -> Result<BlockRegistry, Failure> {
    let src = std::fs::read_to_string(BLOCKS_RON).map_err(|e| {
        (
            2,
            format!("cannot read {BLOCKS_RON}: {e} (run mc-import from the repository root)"),
        )
    })?;
    BlockRegistry::from_ron(&src).map_err(|e| (2, format!("{BLOCKS_RON}: {e}")))
}

fn file_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

fn file_stem(path: &str) -> String {
    Path::new(path)
        .file_stem()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The file's bytes and the volume read from them (the bytes are dropped on return).
fn load(path: &str) -> Result<(BlockVolume, String), Failure> {
    let bytes = std::fs::read(path).map_err(|e| (1, format!("cannot read: {e}")))?;
    let hash = format!("{:08x}", crc32fast::hash(&bytes));
    let volume = read_schem(&bytes).map_err(|e| (1, e.to_string()))?;
    Ok((volume, hash))
}

fn non_air_cells(v: &BlockVolume) -> u64 {
    let solid: Vec<bool> = v
        .palette
        .iter()
        .map(|s| {
            !matches!(
                s.name.as_str(),
                "minecraft:air" | "minecraft:cave_air" | "minecraft:void_air"
            )
        })
        .collect();
    v.blocks
        .iter()
        .filter(|&&i| solid.get(i as usize).copied().unwrap_or(false))
        .count() as u64
}

fn report(path: &str) -> Result<bool, Failure> {
    let (v, hash) = load(path)?;
    let table = Table::builtin();
    let sp = split(&v, table, &SplitOptions::default());
    let [x, y, z] = v.size;
    println!(
        "{}  {x}\u{d7}{y}\u{d7}{z}  data {}  author \"{}\"  non-air {}  ground {}  builds {}{}",
        file_name(path),
        v.data_version,
        v.author,
        non_air_cells(&v),
        sp.ground_layers,
        sp.builds.len(),
        if sp.builds.len() < 2 {
            "  \u{2192} whole"
        } else {
            ""
        }
    );
    let opts = ConvertOptions {
        name: "report".into(),
        air: AirMode::Keep,
        region: None,
        author: v.author.clone(),
        source: file_name(path),
        licence: String::new(),
        source_hash: hash,
        tags: Vec::new(),
        max_records: None,
    };
    match convert(&v, table, &opts) {
        Err(e) => {
            eprintln!("  not convertible: {e}");
            return Ok(false);
        }
        Ok(Some((_, r))) => {
            let mut top: Vec<(&String, &u64)> = r.unmapped.iter().collect();
            top.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
            if !top.is_empty() {
                let list: Vec<String> = top
                    .iter()
                    .take(TOP_UNMAPPED)
                    .map(|(k, n)| format!("{k} \u{d7}{n}"))
                    .collect();
                println!("  unmapped ({} kinds): {}", top.len(), list.join(", "));
            }
            if !r.dropped.is_empty() {
                let list: Vec<String> = r
                    .dropped
                    .iter()
                    .map(|(k, n)| format!("{k} \u{d7}{n}"))
                    .collect();
                println!("  dropped: {}", list.join(", "));
            }
            if r.signs_unparsed > 0 {
                println!("  sign lines unparsed: {}", r.signs_unparsed);
            }
        }
        Ok(None) => println!("  nothing convertible"),
    }
    Ok(true)
}

/// `--out` as an absolute, lexically normalised path (no `..`), to see whether it lies under `assets`.
fn under_assets(out: &Path) -> bool {
    let abs = if out.is_absolute() {
        out.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(out)
    };
    let mut parts: Vec<Component> = Vec::new();
    for c in abs.components() {
        match c {
            Component::ParentDir => {
                parts.pop();
            }
            Component::CurDir => {}
            c => parts.push(c),
        }
    }
    parts
        .iter()
        .any(|c| matches!(c, Component::Normal(n) if n.eq_ignore_ascii_case("assets")))
}

/// The prefab names of a split: `<prefix>-<nn>` by build position; with `use_labels`,
/// `<prefix>-<slug of the label>` for a build that has one.
fn split_names(prefix: &str, builds: &[Build], use_labels: bool) -> Vec<String> {
    let width = if builds.len() >= 100 { 3 } else { 2 };
    let mut taken: BTreeSet<String> = BTreeSet::new();
    builds
        .iter()
        .enumerate()
        .map(|(i, b)| {
            let labelled = b
                .label
                .as_deref()
                .filter(|_| use_labels)
                .map(slug)
                .filter(|s| !s.is_empty())
                .map(|s| format!("{prefix}-{s}"));
            // One uniqueness step for every name: a label such as "03" can collide with a numbered one.
            let wanted = labelled.unwrap_or_else(|| format!("{prefix}-{:0width$}", i + 1));
            let mut name = wanted.clone();
            let mut n = 2;
            while taken.contains(&name) {
                name = format!("{wanted}-{n}");
                n += 1;
            }
            taken.insert(name.clone());
            name
        })
        .collect()
}

fn warn(message: String) {
    eprintln!("warning: {message}");
}

fn write_prefabs(
    is_split: bool,
    path: &str,
    o: &Opts,
    registry: &BlockRegistry,
) -> Result<bool, Failure> {
    let stem = file_stem(path);
    let out_dir = match &o.out {
        Some(d) => PathBuf::from(d),
        None => Path::new(DEFAULT_OUT).join(&stem),
    };
    if under_assets(&out_dir) {
        return Err((
            2,
            format!(
                "{}: refusing to write under assets/ (imports go to {DEFAULT_OUT}/)",
                out_dir.display()
            ),
        ));
    }
    let (v, hash) = load(path)?;
    let table = Table::builtin();
    let base = slug(o.name.as_deref().unwrap_or(&stem));
    let base = if base.is_empty() {
        "prefab".to_string()
    } else {
        base
    };

    // (name, region) per prefab to write.
    let mut jobs: Vec<Job> = Vec::new();
    if is_split {
        let mut so = SplitOptions::default();
        if let Some(g) = o.gap {
            so.gap = g;
        }
        if let Some(m) = o.min_blocks {
            so.min_blocks = m;
        }
        // Without an explicit --ground (an explicit `--ground auto` counts as asked), fewer than two
        // builds means a single building or decoration: import the whole file instead.
        let explicit = o.ground.is_some();
        let ground = o.ground.unwrap_or(GroundMode::Auto);
        so.ground = ground;
        let sp = split(&v, table, &so);
        if !explicit && sp.builds.len() < 2 {
            eprintln!("note: {}: one build, imported whole", file_name(path));
            return finish(path, &v, &hash, vec![(base, None)], o, registry, &out_dir);
        }
        let auto = ground == GroundMode::Auto;
        if sp.builds.is_empty() {
            warn(format!("{}: split found no builds", file_name(path)));
        }
        if auto && sp.ground_layers == v.size[1] {
            warn(format!(
                "{}: automatic ground detection took all {} layers; try --ground none",
                file_name(path),
                v.size[1]
            ));
        } else if auto && sp.ground_layers > 0 && sp.builds.len() == 1 {
            warn(format!(
                "{}: automatic ground took {} layers and one build is left (a single monument on a plate?); try --ground none or import",
                file_name(path),
                sp.ground_layers
            ));
        }
        let names = split_names(&base, &sp.builds, o.labels);
        for i in 0..sp.builds.len() {
            for j in i + 1..sp.builds.len() {
                let (a, b) = (&sp.builds[i], &sp.builds[j]);
                if a.min[0] < b.max[0]
                    && b.min[0] < a.max[0]
                    && a.min[2] < b.max[2]
                    && b.min[2] < a.max[2]
                {
                    warn(format!(
                        "builds {} and {} overlap in x/z",
                        names[i], names[j]
                    ));
                }
            }
        }
        for (n, b) in names.into_iter().zip(&sp.builds) {
            jobs.push((n, Some((b.min, b.max))));
        }
    } else {
        jobs.push((base, None));
    }

    finish(path, &v, &hash, jobs, o, registry, &out_dir)
}

type Job = (String, Option<([u32; 3], [u32; 3])>);

/// Converts, resolves and writes each job; a job that fails marks the run failed and the next one goes on.
fn finish(
    path: &str,
    v: &BlockVolume,
    hash: &str,
    jobs: Vec<Job>,
    o: &Opts,
    registry: &BlockRegistry,
    out_dir: &Path,
) -> Result<bool, Failure> {
    let table = Table::builtin();
    let mut tags: Vec<String> = Vec::new();
    for t in o.tags.iter().map(String::as_str).chain(["imported"]) {
        if !tags.iter().any(|x| x == t) {
            tags.push(t.to_string());
        }
    }
    let mut ok = true;
    let mut made_dir = false;
    for (name, region) in jobs {
        let opts = ConvertOptions {
            name: name.clone(),
            air: o.air,
            region,
            author: o.author.clone().unwrap_or_else(|| v.author.clone()),
            source: file_name(path),
            licence: o.licence.clone(),
            source_hash: hash.to_string(),
            tags: tags.clone(),
            max_records: None,
        };
        let (file, report) = match convert(v, table, &opts) {
            Ok(Some(done)) => done,
            Ok(None) if region.is_some() => {
                eprintln!("{name}: nothing to convert, skipped");
                continue;
            }
            Ok(None) => {
                warn(format!("{}: nothing to convert", file_name(path)));
                ok = false;
                continue;
            }
            Err(e) => {
                eprintln!("{name}: {e}");
                ok = false;
                continue;
            }
        };
        // A prefab that does not resolve is never written.
        if let Err(problems) = resolve(&file, &|n| registry.id_of(n), Trust::Builtin) {
            eprintln!(
                "{name}: does not resolve, {} problem(s), not written:",
                problems.len()
            );
            for p in &problems {
                eprintln!("  {p}");
            }
            ok = false;
            continue;
        }
        if !made_dir {
            match std::fs::create_dir_all(out_dir) {
                Ok(()) => made_dir = true,
                Err(e) => {
                    eprintln!("{name}: {}: {e}", out_dir.display());
                    ok = false;
                    continue;
                }
            }
        }
        let bin = out_dir.join(format!("{name}{BIN_SUFFIX}"));
        let txt = out_dir.join(format!("{name}{REPORT_SUFFIX}"));
        let written = std::fs::write(&bin, prefab_to_bin(&file))
            .map_err(|e| format!("{}: {e}", bin.display()))
            .and_then(|()| {
                std::fs::write(&txt, report.to_text(&name))
                    .map_err(|e| format!("{}: {e}", txt.display()))
            });
        if let Err(e) = written {
            eprintln!("{name}: {e}");
            ok = false;
            continue;
        }
        summary_line(&name, &file.size, &report, file.markers.len());
        let unmapped: u64 = report.unmapped.values().sum();
        if unmapped > 0 || report.signs_unparsed > 0 {
            warn(format!(
                "{name}: {unmapped} unmapped cells, {} unparsed sign lines",
                report.signs_unparsed
            ));
        }
    }
    Ok(ok)
}

fn summary_line(name: &str, size: &[u32; 3], r: &Report, markers: usize) {
    let unmapped: u64 = r.unmapped.values().sum();
    println!(
        "{name}  {}x{}x{}  {} blocks  {} boxes  {} shaped  {} carved  {markers} markers  {unmapped} unmapped",
        size[0], size[1], size[2], r.blocks, r.boxes, r.shaped, r.carved
    );
}
