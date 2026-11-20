//! Creature colliders that follow their `.bbmodel`, in the REST pose.
//!
//! The shapes are computed offline by `tools/creatures/build.py` (every cube
//! corner through its bones' rest rotations) and written as plain decimals
//! into `assets/models/creatures/creature_colliders.json` (`{id: {move,
//! body}}`, compact; the creature lint checks it against the manifest
//! `creatures.json`). The sim never loads a `.bbmodel` for this: `bbmodel`
//! rotates cubes with std `sin`/`cos`, which differ between native and
//! wasm32. The file is embedded here and parsed once; server and client parse the same text, so
//! the shapes are bit-identical and no transcendental runs in the sim.
//!
//! Units are world units (1 Blockbench px = 1 unit, 16 per block), model
//! space: feet on y = 0, facing −Z. Animation does not move these shapes —
//! that would need sim-side clip playback per creature as rollback state.
//!
//! No entity uses them yet: they wait for the first DF creature entity kind.
use std::collections::BTreeMap;
use std::sync::OnceLock;

use rapier3d::math::Vector;

use crate::{ColliderSpec, Pose};

/// The smallest move-capsule radius, in world units (1/8 block): a mouse
/// still gets a capsule the character controller can sweep.
pub const CREATURE_MIN_RADIUS: f32 = 2.0;
/// The largest move-capsule radius, in world units (3/4 block): fortress
/// movement is tile-scale, so a broad animal overhangs its capsule rather
/// than jamming in one-block corridors. `build.py` mirrors both bounds.
pub const CREATURE_MAX_RADIUS: f32 = 12.0;
/// A creature body's density, in `mass_props::BODY_DEFAULT_DENSITY`'s units
/// (kg per cubic world unit): ~1000 kg/m³, water-like flesh, at 16 units
/// per metre. The body's per-bone boxes overlap and bound rotated cubes
/// loosely, so the mass they give is an upper bound (the stylised dog comes
/// out at ~187 kg); tune this when a creature body is first used.
pub const CREATURE_DENSITY: f32 = 1000.0 / (16.0 * 16.0 * 16.0);

/// A creature's two sim shapes.
#[derive(Debug, Clone, PartialEq)]
pub struct CreatureShape {
    /// One upright `CapsuleY` for the character controller (it sweeps a
    /// single yaw-free shape): radius = the smaller horizontal half-extent of
    /// the rest bounds clamped to [`CREATURE_MIN_RADIUS`]..=
    /// [`CREATURE_MAX_RADIUS`], as tall as the bounds. Long animals overhang it.
    pub move_shape: ColliderSpec,
    /// A `Compound` of one cuboid per bone with cubes (the box of that bone's
    /// rest-pose cube corners), in bone order, for the creature as a dynamic
    /// body (a corpse, a thrown animal); its mass is
    /// `mass_props::mass_properties(&body, CREATURE_DENSITY)`.
    pub body: ColliderSpec,
}

/// The shapes of DF creature `df_id` (a `creatures.json` manifest key), or `None` for
/// a creature with no model.
pub fn creature_shape(df_id: &str) -> Option<&'static CreatureShape> {
    shapes().get(df_id)
}

static COLLIDERS: &str = include_str!("../../../assets/models/creatures/creature_colliders.json");

#[derive(serde::Deserialize)]
struct Collider {
    #[serde(rename = "move")]
    move_shape: Capsule,
    body: Vec<Box3>,
}

#[derive(serde::Deserialize)]
struct Capsule {
    radius: f32,
    half_height: f32,
}

#[derive(serde::Deserialize)]
struct Box3 {
    min: [f32; 3],
    max: [f32; 3],
}

fn shapes() -> &'static BTreeMap<String, CreatureShape> {
    static SHAPES: OnceLock<BTreeMap<String, CreatureShape>> = OnceLock::new();
    SHAPES.get_or_init(|| {
        let entries: BTreeMap<String, Collider> = serde_json::from_str(COLLIDERS)
            .expect("creature_colliders.json is malformed (regenerate: just creatures)");
        entries
            .into_iter()
            .map(|(id, c)| (id, shape_of(c)))
            .collect()
    })
}

fn shape_of(c: Collider) -> CreatureShape {
    let body = c
        .body
        .iter()
        .map(|b| {
            let [lo, hi] = [Vector::from_array(b.min), Vector::from_array(b.max)];
            (
                Pose::from_translation((lo + hi) * 0.5),
                ColliderSpec::Cuboid {
                    half_extents: (hi - lo) * 0.5,
                },
            )
        })
        .collect();
    CreatureShape {
        move_shape: ColliderSpec::CapsuleY {
            half_height: c.move_shape.half_height,
            radius: c.move_shape.radius,
        },
        body: ColliderSpec::Compound(body),
    }
}
