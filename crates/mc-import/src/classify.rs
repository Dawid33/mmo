//! Deciding what a Minecraft block state becomes in our game.

use crate::BlockState;
use crate::table::{Table, best};
use game::prefab::{Facing, Half, Shape, Sides, StairCorner};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Class {
    /// air, cave_air, void_air.
    Air,
    /// structure_void, barrier, light: "not part of the build".
    Keep,
    /// A whole block of ours, by name.
    Block {
        block: String,
    },
    Shaped {
        shape: Shape,
        block: String,
    },
    /// A door, torch, chest...; `solid` = also place this block.
    Marker {
        kind: String,
        turn: u8,
        solid: Option<String>,
    },
    /// Water, lava, plants, redstone...: becomes air, counted.
    Dropped {
        why: &'static str,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Classified {
    pub class: Class,
    /// False when the fallback block was used.
    pub mapped: bool,
}

pub fn classify(state: &BlockState, table: &Table) -> Classified {
    let mut name = state.name.strip_prefix("minecraft:").unwrap_or(&state.name);
    for p in &table.strip_prefixes {
        if let Some(rest) = name.strip_prefix(p.as_str()) {
            name = rest;
            break;
        }
    }
    let name = table.aliases.get(name).map(String::as_str).unwrap_or(name);
    let props = &state.props;
    let ok = |class| Classified {
        class,
        mapped: true,
    };

    if matches!(name, "air" | "cave_air" | "void_air") {
        return ok(Class::Air);
    }

    let shaped = |base: Option<&str>, shape: Shape| match base {
        Some(b) => Classified {
            class: Class::Shaped {
                shape,
                block: b.to_string(),
            },
            mapped: true,
        },
        None => Classified {
            class: Class::Shaped {
                shape,
                block: table.fallback.clone(),
            },
            mapped: false,
        },
    };
    let whole = |base: Option<&str>| match base {
        Some(b) => Classified {
            class: Class::Block {
                block: b.to_string(),
            },
            mapped: true,
        },
        None => Classified {
            class: Class::Block {
                block: table.fallback.clone(),
            },
            mapped: false,
        },
    };
    let base_of = |x: &str| -> Option<&str> {
        [
            x.to_string(),
            format!("{x}s"),
            format!("{x}_planks"),
            format!("{x}_block"),
            format!("{x}_wool"),
        ]
        .iter()
        .find_map(|k| table.materials.get(k).map(String::as_str))
    };

    // Keep, dropped and markers compete by specificity of their best match; ties go in this order.
    let keep = best(&table.keep, name).map(|(s, _)| s);
    let dropped = best(&table.dropped, name);
    let marker = best(&table.markers, name);
    let top = [keep, dropped.map(|(s, _)| s), marker.map(|(s, _)| s)]
        .into_iter()
        .flatten()
        .max();
    if let Some(top) = top {
        if keep == Some(top) {
            return ok(Class::Keep);
        }
        if let Some((s, why)) = dropped {
            if s == top {
                return ok(Class::Dropped { why });
            }
        }
        if let Some((_, rule)) = marker {
            // A door is two cells; only the lower one is the marker, the upper is the top of the opening.
            if rule.kind == "door" && props.get("half").map(String::as_str) == Some("upper") {
                return ok(Class::Air);
            }
            // A closed gate is a fence of its wood across its facing; only an open one is a marker.
            if rule.kind == "gate" && props.get("open").map(String::as_str) != Some("true") {
                let across_x = matches!(facing_of(props), Facing::North | Facing::South);
                let sides = Sides {
                    north: !across_x,
                    east: across_x,
                    south: !across_x,
                    west: across_x,
                };
                return shaped(
                    base_of(name.strip_suffix("_fence_gate").unwrap_or(name)),
                    Shape::Fence { sides },
                );
            }
            return ok(Class::Marker {
                kind: rule.kind.clone(),
                turn: turn_of(props),
                solid: rule.solid.clone(),
            });
        }
    }

    if name == "snow" {
        let layers: u8 = props
            .get("layers")
            .and_then(|v| v.parse().ok())
            .unwrap_or(1);
        return if layers >= 8 {
            ok(Class::Block {
                block: "snow".into(),
            })
        } else {
            ok(Class::Shaped {
                shape: Shape::Layer {
                    layers: layers.max(1),
                },
                block: "snow".into(),
            })
        };
    }
    if let Some(x) = name.strip_suffix("_slab") {
        let base = base_of(x);
        return match props.get("type").map(String::as_str) {
            Some("double") => whole(base),
            Some("top") => shaped(base, Shape::Slab { half: Half::Top }),
            _ => shaped(base, Shape::Slab { half: Half::Bottom }),
        };
    }
    if let Some(x) = name.strip_suffix("_stairs") {
        let corner = match props.get("shape").map(String::as_str) {
            Some("inner_left") => StairCorner::InnerLeft,
            Some("inner_right") => StairCorner::InnerRight,
            Some("outer_left") => StairCorner::OuterLeft,
            Some("outer_right") => StairCorner::OuterRight,
            _ => StairCorner::Straight,
        };
        let shape = Shape::Stairs {
            facing: facing_of(props),
            half: half_of(props),
            corner,
        };
        return shaped(base_of(x), shape);
    }
    if let Some(x) = name.strip_suffix("_wall") {
        let side = |k: &str| {
            matches!(
                props.get(k).map(String::as_str),
                Some("low" | "tall" | "true")
            )
        };
        let sides = Sides {
            north: side("north"),
            east: side("east"),
            south: side("south"),
            west: side("west"),
        };
        let post = props.get("up").map(String::as_str) != Some("false");
        // No post and no side is an empty shape (an "invisible wall"): not part of the build.
        if !post && !(sides.north || sides.east || sides.south || sides.west) {
            return ok(Class::Keep);
        }
        return shaped(base_of(x), Shape::Wall { sides, post });
    }
    let flag_sides = || {
        let side = |k: &str| props.get(k).map(String::as_str) == Some("true");
        Sides {
            north: side("north"),
            east: side("east"),
            south: side("south"),
            west: side("west"),
        }
    };
    if let Some(x) = name.strip_suffix("_fence") {
        return shaped(
            base_of(x),
            Shape::Fence {
                sides: flag_sides(),
            },
        );
    }
    if name == "iron_bars" {
        return shaped(
            base_of("iron"),
            Shape::Pane {
                sides: flag_sides(),
            },
        );
    }
    if let Some(x) = name.strip_suffix("_pane") {
        return shaped(
            base_of(x),
            Shape::Pane {
                sides: flag_sides(),
            },
        );
    }
    if let Some(x) = name.strip_suffix("_trapdoor") {
        let open = props.get("open").map(String::as_str) == Some("true");
        return shaped(
            base_of(x),
            Shape::Trapdoor {
                facing: facing_of(props),
                half: half_of(props),
                open,
            },
        );
    }
    if let Some(x) = name.strip_suffix("_carpet") {
        return shaped(base_of(x), Shape::Carpet);
    }
    whole(table.materials.get(name).map(String::as_str))
}

fn parse_facing(v: &str) -> Option<Facing> {
    match v {
        "north" => Some(Facing::North),
        "east" => Some(Facing::East),
        "south" => Some(Facing::South),
        "west" => Some(Facing::West),
        _ => None,
    }
}

fn facing_of(props: &BTreeMap<String, String>) -> Facing {
    props
        .get("facing")
        .and_then(|v| parse_facing(v))
        .unwrap_or(Facing::North)
}

fn half_of(props: &BTreeMap<String, String>) -> Half {
    match props.get("half").map(String::as_str) {
        Some("top") => Half::Top,
        _ => Half::Bottom,
    }
}

/// Quarter turns clockwise from north: from `rotation` (16 steps, 0 = south) or `facing`; 0 without either.
fn turn_of(props: &BTreeMap<String, String>) -> u8 {
    if let Some(r) = props.get("rotation").and_then(|v| v.parse::<u32>().ok()) {
        return (((r % 16) / 4 + 2) % 4) as u8;
    }
    facing_of(props).quarter_turns()
}
