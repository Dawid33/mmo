//! Keyframe animations, sampled the way Blockbench's preview plays them
//! (`js/animations/timeline_animators.js` `BoneAnimator.interpolate` and
//! `displayFrame`, `keyframe.js` `getLerp` / `getCatmullromLerp` /
//! `getBezierLerp`):
//!
//! - per bone, three channels: rotation (Euler degrees **added** to the
//!   bone's rest rotation, then turned into a rotation in `ZYX` order — not a
//!   quaternion multiply, so keys more than 180 degrees apart spin the long
//!   way exactly as authored), position (world units added to the rest
//!   offset) and scale (multiplied);
//! - between the two keyframes around the time: linear, step (hold the
//!   earlier key), Catmull-Rom (through the neighbouring keys, as three.js
//!   `SplineCurve` does) or Bézier (with the keys' handles); before the first
//!   or after the last key the nearest key holds;
//! - a keyframe with two data points is a discontinuity: arriving uses the
//!   first (pre), leaving the second (post);
//! - values are Molang (numbers or expressions of `query.anim_time` etc.);
//! - files saved before format 5.0 have position X and rotation X/Y negated
//!   when read, as Blockbench does on load.
//!
//! Loop modes: `loop` wraps time, `hold` stops on the last frame, `once`
//! stops too and reports itself finished (the game decides what follows).

use std::collections::HashMap;

use serde_json::Value;

use crate::molang::{self, Context, Expr};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopMode {
    Once,
    Hold,
    Loop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Interpolation {
    Linear,
    Step,
    CatmullRom,
    Bezier,
}

#[derive(Debug, Clone)]
pub struct Keyframe {
    pub time: f32,
    pub interpolation: Interpolation,
    /// One or two (pre, post) data points of x, y, z.
    pub data_points: Vec<[Expr; 3]>,
    pub bezier_left_time: [f32; 3],
    pub bezier_left_value: [f32; 3],
    pub bezier_right_time: [f32; 3],
    pub bezier_right_value: [f32; 3],
}

impl Keyframe {
    fn calc(&self, axis: usize, data_point: usize, cx: &Context) -> f32 {
        let i = data_point.min(self.data_points.len().saturating_sub(1));
        self.data_points.get(i).map_or(0.0, |d| d[axis].eval(cx))
    }
}

#[derive(Debug, Clone, Default)]
pub struct BoneTrack {
    /// Index into `Model::nodes`.
    pub node: usize,
    pub rotation: Vec<Keyframe>,
    pub position: Vec<Keyframe>,
    pub scale: Vec<Keyframe>,
}

#[derive(Debug, Clone)]
pub struct Animation {
    pub name: String,
    pub loop_mode: LoopMode,
    /// Seconds.
    pub length: f32,
    pub tracks: Vec<BoneTrack>,
}

/// One bone's animated offsets at a moment.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BonePose {
    /// Degrees added to the rest rotation's Euler angles.
    pub rotation: [f32; 3],
    /// World units added to the rest translation.
    pub position: [f32; 3],
    /// Multiplies the bone's scale.
    pub scale: [f32; 3],
}

impl Default for BonePose {
    fn default() -> Self {
        Self {
            rotation: [0.0; 3],
            position: [0.0; 3],
            scale: [1.0; 3],
        }
    }
}

/// Per-component blend of two bone pose sets (`w` = 0 is `a`, 1 is `b`), as
/// a crossfade plays it; a bone missing from one side blends against its rest
/// offsets. Plain arithmetic in node order, so the simulation and the
/// renderer get the same pose.
pub fn blend(a: &[(usize, BonePose)], b: &[(usize, BonePose)], w: f32) -> Vec<(usize, BonePose)> {
    let mut nodes: Vec<usize> = a.iter().chain(b).map(|(n, _)| *n).collect();
    nodes.sort_unstable();
    nodes.dedup();
    let get = |set: &[(usize, BonePose)], n| {
        set.iter()
            .find(|(m, _)| *m == n)
            .map(|(_, p)| *p)
            .unwrap_or_default()
    };
    let lerp = |x: [f32; 3], y: [f32; 3]| [0, 1, 2].map(|i| x[i] + (y[i] - x[i]) * w);
    nodes
        .into_iter()
        .map(|n| {
            let (p, q) = (get(a, n), get(b, n));
            (
                n,
                BonePose {
                    rotation: lerp(p.rotation, q.rotation),
                    position: lerp(p.position, q.position),
                    scale: lerp(p.scale, q.scale),
                },
            )
        })
        .collect()
}

const EPSILON: f32 = 1.0 / 1200.0;

impl Animation {
    /// The animation's own clock for `elapsed` seconds of playback.
    pub fn local_time(&self, elapsed: f32) -> f32 {
        match self.loop_mode {
            LoopMode::Loop if self.length > 0.0 => elapsed.rem_euclid(self.length),
            _ => elapsed.min(self.length.max(0.0)),
        }
    }

    /// Whether a `once` animation has played out after `elapsed` seconds.
    pub fn finished(&self, elapsed: f32) -> bool {
        self.loop_mode == LoopMode::Once && elapsed >= self.length
    }

    /// Every animated bone's pose at animation time `time` (see
    /// [`Animation::local_time`]), as `(node, pose)`.
    pub fn sample(&self, time: f32, variables: &Context) -> Vec<(usize, BonePose)> {
        let mut cx = variables.clone();
        cx.anim_time = time;
        self.tracks
            .iter()
            .map(|t| {
                let mut pose = BonePose::default();
                if let Some(v) = interpolate(&t.rotation, time, &cx) {
                    pose.rotation = v;
                }
                if let Some(v) = interpolate(&t.position, time, &cx) {
                    pose.position = v;
                }
                if let Some(v) = interpolate(&t.scale, time, &cx) {
                    pose.scale = v;
                }
                (t.node, pose)
            })
            .collect()
    }
}

/// `BoneAnimator.interpolate` for one channel; `None` when it has no keys.
fn interpolate(keys: &[Keyframe], time: f32, cx: &Context) -> Option<[f32; 3]> {
    let mut before: Option<&Keyframe> = None;
    let mut after: Option<&Keyframe> = None;
    for k in keys {
        if k.time < time {
            if before.is_none_or(|b| k.time > b.time) {
                before = Some(k);
            }
        } else if after.is_none_or(|a| k.time < a.time) {
            after = Some(k);
        }
    }
    let at = |k: &Keyframe| {
        // A key the playhead sits on or before arrives with its pre point.
        let dp = if k.time > time || (k.time - time).abs() < EPSILON {
            0
        } else {
            k.data_points.len() - 1
        };
        [0, 1, 2].map(|a| k.calc(a, dp, cx))
    };
    let (b, a) = match (before, after) {
        (None, None) => return None,
        (Some(b), _) if (b.time - time).abs() < EPSILON => return Some(at(b)),
        (_, Some(a)) if (a.time - time).abs() < EPSILON => return Some(at(a)),
        (Some(b), _) if b.interpolation == Interpolation::Step => return Some(at(b)),
        (Some(b), None) => return Some(at(b)),
        (None, Some(a)) => return Some(at(a)),
        (Some(b), Some(a)) => (b, a),
    };
    let alpha = if a.time > b.time {
        (time - b.time) / (a.time - b.time)
    } else {
        0.0
    };
    use Interpolation::*;
    Some(
        if b.interpolation == Linear && matches!(a.interpolation, Linear | Step) {
            // getLerp: leave `b` by its post point, reach `a` by its pre point.
            let b_dp = if b.data_points.len() > 1 { 1 } else { 0 };
            [0, 1, 2].map(|i| {
                let x = b.calc(i, b_dp, cx);
                x + (a.calc(i, 0, cx) - x) * alpha
            })
        } else if b.interpolation == CatmullRom || a.interpolation == CatmullRom {
            let mut sorted: Vec<&Keyframe> = keys.iter().collect();
            sorted.sort_by(|x, y| x.time.total_cmp(&y.time));
            let i = sorted.iter().position(|k| std::ptr::eq(*k, b)).unwrap_or(0);
            let before_plus = i.checked_sub(1).map(|j| sorted[j]);
            let after_plus = sorted.get(i + 2).copied();
            [0, 1, 2].map(|axis| catmull_rom(before_plus, b, a, after_plus, axis, alpha, cx))
        } else {
            [0, 1, 2].map(|axis| bezier(b, a, axis, alpha, cx))
        },
    )
}

/// `getCatmullromLerp`: three.js `SplineCurve` through the neighbouring keys,
/// parametrised by point index (not time), returning the value.
fn catmull_rom(
    before_plus: Option<&Keyframe>,
    before: &Keyframe,
    after: &Keyframe,
    after_plus: Option<&Keyframe>,
    axis: usize,
    alpha: f32,
    cx: &Context,
) -> f32 {
    let mut pts: Vec<f32> = Vec::with_capacity(4);
    let lead = before_plus.filter(|_| before.data_points.len() == 1);
    if let Some(k) = lead {
        pts.push(k.calc(axis, 1, cx));
    }
    pts.push(before.calc(axis, 1, cx));
    pts.push(after.calc(axis, 0, cx));
    if let Some(k) = after_plus.filter(|_| after.data_points.len() == 1) {
        pts.push(k.calc(axis, 0, cx));
    }
    let n = pts.len();
    let t = (alpha + if lead.is_some() { 1.0 } else { 0.0 }) / (n - 1) as f32;
    // SplineCurve.getPoint
    let p = (n - 1) as f32 * t;
    let ip = p.floor() as usize;
    let w = p - ip as f32;
    let p0 = pts[if ip == 0 { ip } else { ip - 1 }];
    let p1 = pts[ip.min(n - 1)];
    let p2 = pts[if ip > n - 2 { n - 1 } else { ip + 1 }];
    let p3 = pts[if ip + 2 > n - 1 { n - 1 } else { ip + 2 }];
    let v0 = (p2 - p0) * 0.5;
    let v1 = (p3 - p1) * 0.5;
    let (t2, t3) = (w * w, w * w * w);
    (2.0 * p1 - 2.0 * p2 + v0 + v1) * t3 + (-3.0 * p1 + 3.0 * p2 - 2.0 * v0 - v1) * t2 + v0 * w + p1
}

/// `getBezierLerp`: the cubic through both keys' handles, sampled at 201
/// points; the value where the curve's time is closest (as Blockbench does).
fn bezier(b: &Keyframe, a: &Keyframe, axis: usize, alpha: f32, cx: &Context) -> f32 {
    let (vb, va) = (b.calc(axis, 1, cx), a.calc(axis, 0, cx));
    let gap = a.time - b.time;
    let hb = b.bezier_right_time[axis].clamp(0.0, gap.max(0.0));
    let ha = a.bezier_left_time[axis].clamp(-gap.max(0.0), 0.0);
    let p = [
        (b.time, vb),
        (b.time + hb, vb + b.bezier_right_value[axis]),
        (a.time + ha, va + a.bezier_left_value[axis]),
        (a.time, va),
    ];
    let time = b.time + gap * alpha;
    let point = |t: f32| {
        let m = 1.0 - t;
        let (k0, k1, k2, k3) = (m * m * m, 3.0 * m * m * t, 3.0 * m * t * t, t * t * t);
        (
            k0 * p[0].0 + k1 * p[1].0 + k2 * p[2].0 + k3 * p[3].0,
            k0 * p[0].1 + k1 * p[1].1 + k2 * p[2].1 + k3 * p[3].1,
        )
    };
    let points: Vec<(f32, f32)> = (0..=200).map(|i| point(i as f32 / 200.0)).collect();
    let mut order: Vec<usize> = (0..points.len()).collect();
    order.sort_by(|&x, &y| {
        (points[x].0 - time)
            .abs()
            .total_cmp(&(points[y].0 - time).abs())
    });
    let (c, s) = (points[order[0]], points[order[1]]);
    let f = if (s.0 - c.0).abs() > 0.0 {
        ((time - c.0) / (s.0 - c.0)).clamp(0.0, 1.0)
    } else {
        0.0
    };
    c.1 + (s.1 - c.1) * f
}

// --- Reading ------------------------------------------------------------------

/// Reads `animations[]`; `bones` maps group uuid to node index. Returns the
/// animations and anything skipped.
pub(crate) fn read(
    animations: &[Value],
    bones: &HashMap<String, usize>,
    format_version: &str,
) -> (Vec<Animation>, Vec<String>) {
    let mut warnings = Vec::new();
    // Blockbench `bbmodel.js`: pre-5.0 files have these axes flipped.
    let old_axes = version_lt(format_version, (5, 0));
    let mut out = Vec::new();
    for anim in animations {
        let name = anim
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("animation")
            .to_string();
        let loop_mode = match anim.get("loop").and_then(Value::as_str) {
            Some("loop") => LoopMode::Loop,
            Some("hold") => LoopMode::Hold,
            _ => LoopMode::Once,
        };
        let mut length = anim.get("length").and_then(Value::as_f64).unwrap_or(0.0) as f32;
        let mut tracks = Vec::new();
        if let Some(Value::Object(animators)) = anim.get("animators") {
            let mut uuids: Vec<&String> = animators.keys().collect();
            uuids.sort();
            for uuid in uuids {
                let animator = &animators[uuid];
                if animator
                    .get("type")
                    .and_then(Value::as_str)
                    .is_some_and(|t| t != "bone")
                {
                    continue; // effects, null objects, cameras
                }
                let Some(&node) = bones.get(uuid.as_str()) else {
                    warnings.push(format!(
                        "animation {name:?}: animator {uuid} has no bone, skipped"
                    ));
                    continue;
                };
                let mut track = BoneTrack {
                    node,
                    ..Default::default()
                };
                for k in animator
                    .get("keyframes")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    let channel = k.get("channel").and_then(Value::as_str).unwrap_or("");
                    let key = match keyframe(k, channel, old_axes) {
                        Ok(key) => key,
                        Err(e) => {
                            warnings.push(format!("animation {name:?}: {e}"));
                            continue;
                        }
                    };
                    length = length.max(key.time);
                    match channel {
                        "rotation" => track.rotation.push(key),
                        "position" => track.position.push(key),
                        "scale" => track.scale.push(key),
                        _ => {}
                    }
                }
                tracks.push(track);
            }
        }
        out.push(Animation {
            name,
            loop_mode,
            length,
            tracks,
        });
    }
    (out, warnings)
}

fn keyframe(k: &Value, channel: &str, old_axes: bool) -> Result<Keyframe, String> {
    let interpolation = match k.get("interpolation").and_then(Value::as_str) {
        Some("step") => Interpolation::Step,
        Some("catmullrom") => Interpolation::CatmullRom,
        Some("bezier") => Interpolation::Bezier,
        _ => Interpolation::Linear,
    };
    let mut data_points = Vec::new();
    for dp in k
        .get("data_points")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let mut xyz = [Expr::Num(0.0), Expr::Num(0.0), Expr::Num(0.0)];
        for (i, axis) in ["x", "y", "z"].iter().enumerate() {
            let mut e = match dp.get(*axis) {
                Some(Value::Number(n)) => Expr::Num(n.as_f64().unwrap_or(0.0) as f32),
                Some(Value::String(s)) => {
                    molang::parse(s).map_err(|e| format!("{channel} {axis}: {e}"))?
                }
                _ => Expr::Num(if channel == "scale" { 1.0 } else { 0.0 }),
            };
            let flip = old_axes
                && ((i == 0 && (channel == "position" || channel == "rotation"))
                    || (i == 1 && channel == "rotation"));
            if flip {
                e = match e {
                    Expr::Num(n) => Expr::Num(-n),
                    other => Expr::Neg(Box::new(other)),
                };
            }
            xyz[i] = e;
        }
        data_points.push(xyz);
    }
    if data_points.is_empty() {
        return Err(format!("{channel} keyframe without data points"));
    }
    let arr = |key: &str, default: f32| -> [f32; 3] {
        let mut out = [default; 3];
        if let Some(Value::Array(a)) = k.get(key) {
            for (o, v) in out.iter_mut().zip(a) {
                *o = v.as_f64().unwrap_or(default as f64) as f32;
            }
        }
        out
    };
    let mut kf = Keyframe {
        time: k.get("time").and_then(Value::as_f64).unwrap_or(0.0) as f32,
        interpolation,
        data_points,
        bezier_left_time: arr("bezier_left_time", -0.1),
        bezier_left_value: arr("bezier_left_value", 0.0),
        bezier_right_time: arr("bezier_right_time", 0.1),
        bezier_right_value: arr("bezier_right_value", 0.0),
    };
    if old_axes && interpolation == Interpolation::Bezier {
        let axes: &[usize] = match channel {
            "rotation" => &[0, 1],
            "position" => &[0],
            _ => &[],
        };
        for &i in axes {
            kf.bezier_left_value[i] *= -1.0;
            kf.bezier_right_value[i] *= -1.0;
        }
    }
    Ok(kf)
}

fn version_lt(v: &str, (major, minor): (u32, u32)) -> bool {
    let mut parts = v.split('.').map(|p| p.parse::<u32>().unwrap_or(0));
    let (a, b) = (parts.next().unwrap_or(0), parts.next().unwrap_or(0));
    (a, b) < (major, minor)
}
