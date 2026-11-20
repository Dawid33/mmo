//! The tuning page: `just forge-ui` serves ui.html on localhost, with every
//! knob (knobs.rs) as a slider and the forged map as a live preview. A change
//! re-forges the world at a coarse preview cell size (64 m is about a second)
//! and swaps the image; nothing is written unless the page asks to save a
//! world file.
//!
//! Plain HTTP/1.1 over std::net, GET only, bound to 127.0.0.1: a development
//! tool, not a service. No dependencies beyond what the forge already uses.
//!
//!   GET /                      the page
//!   GET /knobs                 knob table, rock legend, base seed
//!   GET /forge?cell=&seed=&<knob>=…   forge, reply with statistics + an id
//!   GET /image?id=&view=       PNG of a forged world (rivers, relief, geology, rain, initial)
//!   GET /export?cell=&seed=&<knob>=…  forge and write `<out>-ui-<seed>.world`

use std::fmt::Write as _;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

use crate::Params;
use crate::{render, search};
use worldgen::forge::geology::ROCKS;
use worldgen::forge::knobs::{CHOICES, SPECS};
use worldgen::forge::render::PreviewLayer;
use worldgen::forge::{ForgeParams, Forged, forge};

const PAGE: &str = include_str!("ui.html");
/// Forged worlds kept for /image (each holds a few rasters; 64 m worlds are tiny).
const KEPT_WORLDS: usize = 4;
/// Preview cell sizes the page may ask for (m). Finer than 16 m is a CLI job.
const CELL_M_BOUNDS: (f64, f64) = (16.0, 128.0);

struct Server {
    base: Params,
    worlds: Mutex<Vec<(String, Arc<Forged>)>>,
    /// One forge at a time: each run already uses a core flat out.
    forging: Mutex<()>,
}

pub fn run(base: &Params, port: u16) {
    let listener = TcpListener::bind(("127.0.0.1", port))
        .unwrap_or_else(|e| panic!("cannot listen on 127.0.0.1:{port}: {e}"));
    println!("forge tuning page: http://127.0.0.1:{port}/   (Ctrl-C to stop)");
    let server = Arc::new(Server {
        base: base.clone(),
        worlds: Mutex::new(Vec::new()),
        forging: Mutex::new(()),
    });
    for stream in listener.incoming().flatten() {
        let server = server.clone();
        std::thread::spawn(move || handle(&server, stream));
    }
}

fn handle(server: &Server, mut stream: TcpStream) {
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(5)));
    let mut buf = Vec::new();
    let mut chunk = [0u8; 2048];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") && buf.len() < 32 * 1024 {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
    let request = String::from_utf8_lossy(&buf);
    let mut parts = request.lines().next().unwrap_or("").split_whitespace();
    let (method, target) = (parts.next().unwrap_or(""), parts.next().unwrap_or("/"));
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let (status, content_type, body) = if method != "GET" {
        (405, "text/plain", b"GET only".to_vec())
    } else {
        route(server, path, query)
    };
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Error",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream
        .write_all(head.as_bytes())
        .and_then(|_| stream.write_all(&body));
}

fn route(server: &Server, path: &str, query: &str) -> (u16, &'static str, Vec<u8>) {
    let json = |status: u16, body: String| (status, "application/json", body.into_bytes());
    let error = |message: String| json(400, format!("{{\"error\": {}}}", quote(&message)));
    match path {
        "/" => (200, "text/html; charset=utf-8", PAGE.as_bytes().to_vec()),
        "/knobs" => json(200, knobs_json(&server.base)),
        "/forge" => match params_from_query(&server.base, query) {
            Ok(p) => json(200, forge_json(server, &p, query)),
            Err(e) => error(e),
        },
        "/export" => match params_from_query(&server.base, query) {
            Ok(p) => json(200, export_json(server, &p)),
            Err(e) => error(e),
        },
        "/image" => {
            let pairs = parse_query(query);
            let find = |key: &str| {
                pairs
                    .iter()
                    .find(|(k, _)| k == key)
                    .map(|(_, v)| v.as_str())
            };
            let (Some(id), view) = (find("id"), find("view").unwrap_or("rivers")) else {
                return error("image needs an id".into());
            };
            let world = server
                .worlds
                .lock()
                .unwrap()
                .iter()
                .find(|(k, _)| k == id)
                .map(|(_, w)| w.clone());
            match world {
                Some(f) => (200, "image/png", render::png_bytes(&f, view_layer(view))),
                None => (
                    404,
                    "text/plain",
                    b"that render is gone; forge again".to_vec(),
                ),
            }
        }
        _ => (404, "text/plain", b"not found".to_vec()),
    }
}

fn view_layer(view: &str) -> PreviewLayer {
    match view {
        "relief" => PreviewLayer::Relief,
        "geology" => PreviewLayer::Geology,
        "rain" => PreviewLayer::Precipitation,
        "initial" => PreviewLayer::BeforeErosion,
        "biome" => PreviewLayer::Biome,
        "temperature" => PreviewLayer::Temperature,
        "drainage" => PreviewLayer::Drainage,
        "regions" => PreviewLayer::Regions,
        _ => PreviewLayer::Rivers,
    }
}

fn parse_query(query: &str) -> Vec<(String, String)> {
    fn decode(s: &str) -> String {
        let bytes = s.as_bytes();
        let (mut out, mut i) = (Vec::with_capacity(bytes.len()), 0);
        while i < bytes.len() {
            match bytes[i] {
                b'+' => out.push(b' '),
                b'%' if i + 2 < bytes.len() => {
                    let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                    match u8::from_str_radix(hex, 16) {
                        Ok(b) => {
                            out.push(b);
                            i += 2;
                        }
                        Err(_) => out.push(b'%'),
                    }
                }
                b => out.push(b),
            }
            i += 1;
        }
        String::from_utf8_lossy(&out).into_owned()
    }
    query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            (decode(k), decode(v))
        })
        .collect()
}

/// The page's query -> forge parameters. Knob values are clamped to their
/// table range, so a typo cannot ask for a million iterations.
fn params_from_query(base: &Params, query: &str) -> Result<Params, String> {
    let mut p = Params {
        forge: ForgeParams {
            quiet: true,
            ..base.forge.clone()
        },
        search: 0,
        serve: None,
        spawn: None,
        report: false,
        ..base.clone()
    };
    for (key, value) in parse_query(query) {
        match key.as_str() {
            "seed" => {
                let hex = value.trim_start_matches("0x").replace('_', "");
                p.seed = u64::from_str_radix(&hex, 16)
                    .map_err(|_| format!("seed \"{value}\" is not hexadecimal"))?;
            }
            "cell" => {
                let cell: f64 = value
                    .parse()
                    .map_err(|_| format!("cell \"{value}\" is not a number"))?;
                p.cell_m = cell.clamp(CELL_M_BOUNDS.0, CELL_M_BOUNDS.1);
            }
            knob => {
                let spec = SPECS
                    .iter()
                    .find(|s| s.key == knob)
                    .ok_or_else(|| format!("no knob named \"{knob}\""))?;
                let number: f64 = value
                    .parse()
                    .map_err(|_| format!("{knob}: \"{value}\" is not a number"))?;
                if !number.is_finite() {
                    return Err(format!("{knob}: \"{value}\" is not a finite number"));
                }
                p.knobs.set(knob, number.clamp(spec.min, spec.max));
            }
        }
    }
    Ok(p)
}

fn quote(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c if (c as u32) < 0x20 => {}
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// JSON number; NaN and infinities become null.
fn num(v: f64) -> String {
    if v.is_finite() {
        format!("{v}")
    } else {
        "null".into()
    }
}

fn knobs_json(base: &Params) -> String {
    let mut s = format!(
        "{{\"seed\": \"{:X}\", \"width_m\": {}, \"height_m\": {}, \"knobs\": [",
        base.seed, base.width_m, base.height_m
    );
    for (i, spec) in SPECS.iter().enumerate() {
        let _ = write!(
            s,
            "{}{{\"key\": {}, \"group\": {}, \"label\": {}, \"help\": {}, \"default\": {}, \"value\": {}, \"min\": {}, \"max\": {}, \"choices\": {}}}",
            if i == 0 { "" } else { ", " },
            quote(spec.key),
            quote(spec.group),
            quote(spec.label),
            quote(spec.help),
            num(spec.default),
            // The value the page starts from: `--set` on the command line wins over the default.
            num(base.knobs.get(spec.key).unwrap_or(spec.default)),
            num(spec.min),
            num(spec.max),
            // A knob that is a choice: the page shows a list, the value is the index.
            match CHOICES.iter().find(|(key, _)| *key == spec.key) {
                Some((_, names)) => format!(
                    "[{}]",
                    names
                        .iter()
                        .map(|n| quote(n))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                None => "null".to_string(),
            },
        );
    }
    s.push_str("], \"rocks\": [");
    for (i, rock) in ROCKS.iter().enumerate() {
        let _ = write!(
            s,
            "{}{{\"name\": {}, \"rgb\": [{}, {}, {}]}}",
            if i == 0 { "" } else { ", " },
            quote(rock.name),
            rock.rgb[0],
            rock.rgb[1],
            rock.rgb[2]
        );
    }
    s.push_str("]}");
    s
}

fn stats_json(f: &Forged, p: &Params) -> String {
    let st = &f.analysis.stats;
    let cells = f.world.cells();
    let list = |values: &[f64]| {
        values
            .iter()
            .map(|&v| num(v))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let artifact = f.artifact(&p.forge);
    let (score, terms) = search::score(f, &artifact);
    let criteria = terms
        .iter()
        .map(|(name, value)| format!("{{\"name\": {}, \"value\": {}}}", quote(name), num(*value)))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "\"cell_m\": {}, \"islands\": {}, \"largest_island_km2\": {}, \"land_share\": {}, \"land_km2\": {}, \"heights\": [{}], \"slopes\": [{}], \
         \"simulated_peak_m\": {}, \"height_fit\": {}, \"lakes\": {}, \"largest_lake_km2\": {}, \
         \"mouths\": {}, \"channel_km\": {}, \"longest_river_km\": {}, \"biggest_basin_km2\": {}, \
         \"max_order\": {}, \"theta\": {}, \"hack\": {}, \"buildable_km2\": {}, \"rock_shares\": [{}], \
         \"margin_share\": {}, \"score\": {}, \"criteria\": [{}]",
        num(f.world.dx),
        st.islands,
        num(st.largest_island_km2),
        num(st.land_share),
        num(st.land_km2),
        list(&st.heights),
        list(&st.slopes),
        num(f.simulated_peak_m),
        num(f.height_fit),
        st.lake_count,
        num(st.largest_lake_km2),
        st.mouths,
        num(st.channel_km),
        num(st.longest_river_km),
        num(st.biggest_basin_km2),
        st.max_order,
        num(st.theta),
        num(st.hack),
        num(st.buildable_km2),
        list(&st.rock_shares),
        num(st.margin_drowned as f64 / cells as f64),
        num(score),
        criteria,
    )
}

fn forge_json(server: &Server, p: &Params, query: &str) -> String {
    let id = format!("{:08x}", crc32fast::hash(query.as_bytes()));
    let cached = server
        .worlds
        .lock()
        .unwrap()
        .iter()
        .find(|(k, _)| *k == id)
        .map(|(_, w)| w.clone());
    let started = std::time::Instant::now();
    let forged = match cached {
        Some(world) => world,
        None => {
            let _one_at_a_time = server.forging.lock().unwrap();
            let world = Arc::new(forge(&p.forge));
            let mut worlds = server.worlds.lock().unwrap();
            worlds.push((id.clone(), world.clone()));
            if worlds.len() > KEPT_WORLDS {
                worlds.remove(0);
            }
            world
        }
    };
    format!(
        "{{\"id\": {}, \"seed\": \"{:X}\", \"ms\": {}, {}}}",
        quote(&id),
        p.seed,
        started.elapsed().as_millis(),
        stats_json(&forged, p)
    )
}

fn export_json(server: &Server, p: &Params) -> String {
    let _one_at_a_time = server.forging.lock().unwrap();
    let started = std::time::Instant::now();
    let forged = forge(&p.forge);
    let artifact = forged.artifact(&p.forge);
    let path = format!(
        "{}-ui-{:X}.world",
        server.base.out.to_string_lossy(),
        p.seed
    );
    if let Some(dir) = std::path::Path::new(&path).parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    match artifact.save(std::path::Path::new(&path)) {
        Ok(()) => {
            let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            println!(
                "saved {path} ({:.1} MiB) from the tuning page",
                bytes as f64 / 1048576.0
            );
            format!(
                "{{\"path\": {}, \"mib\": {}, \"ms\": {}, \"spawn\": [{}, {}], {}}}",
                quote(&path),
                num(bytes as f64 / 1048576.0),
                started.elapsed().as_millis(),
                num(artifact.spawn[0]),
                num(artifact.spawn[1]),
                stats_json(&forged, p)
            )
        }
        Err(e) => format!(
            "{{\"error\": {}}}",
            quote(&format!("could not write {path}: {e}"))
        ),
    }
}
