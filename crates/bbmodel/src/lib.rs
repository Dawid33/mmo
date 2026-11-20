//! Reads Blockbench project files (`.bbmodel`) into engine-free geometry:
//! a node tree (one node per group, plus the root) whose nodes carry
//! triangle parts, one per texture, in the node's local space.
//!
//! Units: one Blockbench pixel is one world unit (16 per block, as the game's
//! voxels), +Y up, north = -Z, east = +X — the same axes Bevy uses, so
//! nothing is scaled or flipped.
//!
//! Matches Blockbench's own preview (checked against its source, JannisX11/
//! blockbench `js/outliner/types/cube.js`, `mesh.js`, `outliner.js`,
//! `js/util/three_custom.js`):
//! - cube faces in `setShape`'s vertex order, UVs per `updateUV` (box UV
//!   recomputed from `uv_offset` + size with the 1/64 px anti-bleed margin,
//!   face UV rotation, per-texture UV size);
//! - Euler rotations in three.js order `ZYX` (X applied first);
//! - groups as bones (`bone_rig` formats): a node sits at its origin
//!   relative to its parent's origin; `java_block` models have no bones, so
//!   their groups only organise and every element is placed absolutely;
//! - a face whose `texture` is `null` is absent; a face with no texture
//!   (`false` or missing) is drawn untextured;
//! - elements or groups with `visibility: false` or `export: false` are
//!   skipped.
//!
//! Group formats: 5.0 files list groups in a top-level `groups[]` and the
//! `outliner` holds `{uuid, children}`; 4.x files put whole group objects in
//! the `outliner`. Both are read.
//!
//! Animations are read and sampled as Blockbench plays them (see
//! [`animation`]); [`Model::local_pose`] turns a sample into each node's
//! local transform.
//!
//! Not read (logged in [`Model::warnings`]): element types other than cube
//! and mesh (locators, null objects, ...), animation effect tracks,
//! animated texture frames (frame 0 = the whole image is used), external
//! texture files unless a resolver is given to [`parse_with`].

use std::collections::HashMap;

pub mod animation;
pub mod molang;

pub use animation::{Animation, BonePose, Interpolation, LoopMode};

#[cfg(feature = "textures")]
use base64::Engine as _;
use serde::Deserialize;
use serde_json::Value;

/// How a texture's alpha should be drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlphaKind {
    /// Every texel fully opaque.
    Opaque,
    /// Only fully opaque or fully transparent texels: alpha test.
    Mask,
    /// Some texel is partly transparent: blend.
    Blend,
}

/// A decoded texture.
#[derive(Debug, Clone)]
pub struct Texture {
    pub name: String,
    pub width: u32,
    pub height: u32,
    /// RGBA8, row-major, `width * height * 4` bytes.
    pub rgba: Vec<u8>,
    pub alpha: AlphaKind,
}

/// Triangles of one node sharing one texture (`None`: untextured).
#[derive(Debug, Clone, Default)]
pub struct Part {
    pub texture: Option<usize>,
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    /// 0..1, origin at the image's top left (as Bevy and wgpu expect).
    pub uvs: Vec<[f32; 2]>,
    pub indices: Vec<u32>,
}

impl Part {
    fn push_vertex(&mut self, p: [f32; 3], n: [f32; 3], uv: [f32; 2]) -> u32 {
        self.positions.push(p);
        self.normals.push(n);
        self.uvs.push(uv);
        (self.positions.len() - 1) as u32
    }

    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }
}

/// One bone: the root (index 0) or a group.
#[derive(Debug, Clone)]
pub struct Node {
    pub name: String,
    pub parent: Option<usize>,
    /// Relative to the parent node, world units.
    pub translation: [f32; 3],
    /// Euler degrees, applied in three.js `ZYX` order (X first); see
    /// [`Node::rotation_quat`].
    pub rotation_deg: [f32; 3],
    pub parts: Vec<Part>,
}

impl Node {
    /// The rotation as a quaternion `[x, y, z, w]`: `Rz * Ry * Rx`, which is
    /// what Bevy's `Quat::from_euler(EulerRot::ZYX, z, y, x)` builds too.
    pub fn rotation_quat(&self) -> [f32; 4] {
        euler_zyx_quat(self.rotation_deg)
    }
}

#[derive(Debug, Clone)]
pub struct Model {
    pub name: String,
    pub format_version: String,
    pub model_format: String,
    pub textures: Vec<Texture>,
    /// `nodes[0]` is the root; a parent always precedes its children.
    pub nodes: Vec<Node>,
    pub animations: Vec<Animation>,
    /// Things the reader skipped or could not honour.
    pub warnings: Vec<String>,
}

#[derive(Debug)]
pub enum Error {
    Json(serde_json::Error),
    Texture { name: String, reason: String },
    Format(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Json(e) => write!(f, "bbmodel json: {e}"),
            Error::Texture { name, reason } => write!(f, "bbmodel texture {name:?}: {reason}"),
            Error::Format(e) => write!(f, "bbmodel: {e}"),
        }
    }
}

impl std::error::Error for Error {}

/// Parse a `.bbmodel` whose textures are all embedded.
pub fn parse(bytes: &[u8]) -> Result<Model, Error> {
    parse_with(bytes, &mut |_| None)
}

/// Parse a `.bbmodel`; `resolve` is asked for the bytes of a texture that is
/// not embedded, by its `relative_path` (falling back to `path`), and may
/// return `None` (then the texture is an error).
pub fn parse_with(
    bytes: &[u8],
    resolve: &mut dyn FnMut(&str) -> Option<Vec<u8>>,
) -> Result<Model, Error> {
    let file: File = serde_json::from_slice(bytes).map_err(Error::Json)?;
    Reader::new(file, Some(resolve))?.read()
}

/// Parse only the rig: bones, geometry and animations, with every texture a
/// pixel-less placeholder (`width` = `height` = 0, no `rgba`) in its slot, so
/// faces, parts and node order are exactly those [`parse`] returns. Never
/// decodes an image; what the deterministic simulation loads.
pub fn parse_rig(bytes: &[u8]) -> Result<Model, Error> {
    let file: File = serde_json::from_slice(bytes).map_err(Error::Json)?;
    Reader::new(file, None)?.read()
}

// --- File schema (only what is read) -----------------------------------------

#[derive(Deserialize, Default)]
#[serde(default)]
struct File {
    meta: Meta,
    name: String,
    resolution: Option<Resolution>,
    elements: Vec<Value>,
    groups: Vec<Value>,
    outliner: Vec<Value>,
    textures: Vec<TextureEntry>,
    animations: Vec<Value>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Meta {
    format_version: String,
    model_format: String,
    box_uv: bool,
}

#[derive(Deserialize, Clone, Copy)]
struct Resolution {
    width: f32,
    height: f32,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct TextureEntry {
    name: String,
    source: Option<String>,
    relative_path: Option<String>,
    path: Option<String>,
    uv_width: Option<f32>,
    uv_height: Option<f32>,
}

#[derive(Deserialize)]
struct Cube {
    #[serde(default)]
    name: String,
    from: [f32; 3],
    to: [f32; 3],
    #[serde(default)]
    origin: [f32; 3],
    #[serde(default)]
    rotation: [f32; 3],
    #[serde(default)]
    inflate: f32,
    #[serde(default = "one3")]
    stretch: [f32; 3],
    box_uv: Option<bool>,
    #[serde(default)]
    uv_offset: [f32; 2],
    #[serde(default)]
    mirror_uv: bool,
    #[serde(default)]
    faces: HashMap<String, Value>,
}

fn one3() -> [f32; 3] {
    [1.0; 3]
}

#[derive(Deserialize)]
struct MeshElement {
    #[serde(default)]
    name: String,
    #[serde(default)]
    origin: [f32; 3],
    #[serde(default)]
    rotation: [f32; 3],
    #[serde(default)]
    vertices: HashMap<String, [f32; 3]>,
    #[serde(default)]
    faces: HashMap<String, MeshFace>,
}

#[derive(Deserialize)]
struct MeshFace {
    #[serde(default)]
    vertices: Vec<String>,
    #[serde(default)]
    uv: HashMap<String, [f32; 2]>,
    /// Missing reads as `false` (untextured); an explicit `null` stays null
    /// (absent face).
    #[serde(default = "untextured")]
    texture: Value,
}

fn untextured() -> Value {
    Value::Bool(false)
}

/// A face's texture field: `null` = absent face, a number = that texture,
/// `false` / missing = drawn untextured.
enum FaceTexture {
    Absent,
    Untextured,
    Index(usize),
}

fn face_texture(v: Option<&Value>) -> FaceTexture {
    match v {
        Some(Value::Null) => FaceTexture::Absent,
        Some(Value::Number(n)) => n
            .as_u64()
            .map_or(FaceTexture::Untextured, |i| FaceTexture::Index(i as usize)),
        _ => FaceTexture::Untextured,
    }
}

fn shown(v: &Value) -> bool {
    v.get("visibility").and_then(Value::as_bool) != Some(false)
        && v.get("export").and_then(Value::as_bool) != Some(false)
}

fn vec3(v: Option<&Value>) -> [f32; 3] {
    let mut out = [0.0; 3];
    if let Some(Value::Array(a)) = v {
        for (o, x) in out.iter_mut().zip(a) {
            *o = x.as_f64().unwrap_or(0.0) as f32;
        }
    }
    out
}

// --- Reading ------------------------------------------------------------------

struct Reader {
    file: File,
    textures: Vec<Texture>,
    /// Per texture: the UV space its face UVs are given in.
    uv_space: Vec<[f32; 2]>,
    default_uv_space: [f32; 2],
    bone_rig: bool,
    elements: HashMap<String, Value>,
    groups: HashMap<String, Value>,
    nodes: Vec<Node>,
    /// Absolute origin per node (bone pivots are absolute in the file).
    origins: Vec<[f32; 3]>,
    placed: std::collections::HashSet<String>,
    /// Group uuid -> node, for animators.
    bones: HashMap<String, usize>,
    warnings: Vec<String>,
}

impl Reader {
    /// `resolve` `None` reads the rig only (placeholder textures).
    fn new(
        file: File,
        mut resolve: Option<&mut dyn FnMut(&str) -> Option<Vec<u8>>>,
    ) -> Result<Self, Error> {
        let res = file.resolution.unwrap_or(Resolution {
            width: 16.0,
            height: 16.0,
        });
        let default_uv_space = [res.width, res.height];
        let mut textures = Vec::new();
        let mut uv_space = Vec::new();
        for t in &file.textures {
            textures.push(match resolve.as_deref_mut() {
                Some(resolve) => decode_texture(t, resolve)?,
                None => placeholder_texture(t),
            });
            uv_space.push([
                t.uv_width.unwrap_or(res.width),
                t.uv_height.unwrap_or(res.height),
            ]);
        }
        let key = |v: &Value| {
            v.get("uuid")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        let elements = file.elements.iter().map(|e| (key(e), e.clone())).collect();
        let groups = file.groups.iter().map(|g| (key(g), g.clone())).collect();
        // Formats without bones (Java block/item) place every element absolutely.
        let bone_rig = !matches!(
            file.meta.model_format.as_str(),
            "java_block" | "optifine_part"
        );
        Ok(Self {
            file,
            textures,
            uv_space,
            default_uv_space,
            bone_rig,
            elements,
            groups,
            nodes: vec![Node {
                name: "root".into(),
                parent: None,
                translation: [0.0; 3],
                rotation_deg: [0.0; 3],
                parts: Vec::new(),
            }],
            origins: vec![[0.0; 3]],
            placed: Default::default(),
            bones: HashMap::new(),
            warnings: Vec::new(),
        })
    }

    fn read(mut self) -> Result<Model, Error> {
        let outliner = std::mem::take(&mut self.file.outliner);
        for item in &outliner {
            self.outliner_item(item, 0);
        }
        // Elements no outliner entry points to still belong to the model.
        let mut stray: Vec<String> = self
            .elements
            .keys()
            .filter(|k| !self.placed.contains(*k))
            .cloned()
            .collect();
        stray.sort();
        for uuid in stray {
            self.element(&uuid, 0);
        }
        let (animations, warnings) = animation::read(
            &self.file.animations,
            &self.bones,
            &self.file.meta.format_version,
        );
        self.warnings.extend(warnings);
        for node in &mut self.nodes {
            node.parts.retain(|p| !p.indices.is_empty());
        }
        Ok(Model {
            name: self.file.name.clone(),
            format_version: self.file.meta.format_version.clone(),
            model_format: self.file.meta.model_format.clone(),
            textures: self.textures,
            nodes: self.nodes,
            animations,
            warnings: self.warnings,
        })
    }

    fn outliner_item(&mut self, item: &Value, parent: usize) {
        match item {
            Value::String(uuid) => self.element(uuid, parent),
            Value::Object(_) => {
                let uuid = item.get("uuid").and_then(Value::as_str).unwrap_or_default();
                // 4.x: the group object is inline (it has a name); 5.0: look it up.
                let group = if item.get("name").is_some() {
                    item.clone()
                } else if let Some(g) = self.groups.get(uuid) {
                    g.clone()
                } else {
                    self.warnings
                        .push(format!("outliner group {uuid} not in groups[]: skipped"));
                    return;
                };
                if !shown(&group) {
                    return;
                }
                let origin = vec3(group.get("origin"));
                let rotation = vec3(group.get("rotation"));
                let parent_origin = self.origins[parent];
                let index = self.nodes.len();
                self.bones.insert(uuid.to_string(), index);
                self.nodes.push(Node {
                    name: group
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("group")
                        .to_string(),
                    parent: Some(parent),
                    translation: if self.bone_rig {
                        sub(origin, parent_origin)
                    } else {
                        [0.0; 3]
                    },
                    rotation_deg: if self.bone_rig { rotation } else { [0.0; 3] },
                    parts: Vec::new(),
                });
                self.origins
                    .push(if self.bone_rig { origin } else { [0.0; 3] });
                if let Some(Value::Array(children)) = item.get("children") {
                    for child in children {
                        self.outliner_item(child, index);
                    }
                }
            }
            _ => {}
        }
    }

    fn element(&mut self, uuid: &str, node: usize) {
        if !self.placed.insert(uuid.to_string()) {
            return;
        }
        let Some(e) = self.elements.get(uuid).cloned() else {
            self.warnings.push(format!(
                "outliner element {uuid} not in elements[]: skipped"
            ));
            return;
        };
        if !shown(&e) {
            return;
        }
        let kind = e.get("type").and_then(Value::as_str).unwrap_or("cube");
        match kind {
            "cube" => match serde_json::from_value::<Cube>(e) {
                Ok(c) => self.cube(&c, node),
                Err(err) => self.warnings.push(format!("cube {uuid}: {err}")),
            },
            "mesh" => match serde_json::from_value::<MeshElement>(e) {
                Ok(m) => self.mesh(&m, node),
                Err(err) => self.warnings.push(format!("mesh {uuid}: {err}")),
            },
            other => self.warnings.push(format!("{other} element skipped")),
        }
    }

    /// Where an element-local point lands in its node's space: rotate about
    /// the element's origin, then offset by the origin relative to the node.
    fn placer(
        &self,
        node: usize,
        origin: [f32; 3],
        rotation: [f32; 3],
    ) -> impl Fn([f32; 3]) -> [f32; 3] + use<> {
        let q = euler_zyx_quat(rotation);
        let offset = sub(origin, self.origins[node]);
        move |p| add(quat_rotate(q, p), offset)
    }

    fn part(&mut self, node: usize, texture: Option<usize>) -> &mut Part {
        let parts = &mut self.nodes[node].parts;
        let i = match parts.iter().position(|p| p.texture == texture) {
            Some(i) => i,
            None => {
                parts.push(Part {
                    texture,
                    ..Default::default()
                });
                parts.len() - 1
            }
        };
        &mut parts[i]
    }

    fn texture_for(&mut self, what: &str, face: &FaceTexture) -> Option<Option<usize>> {
        match *face {
            FaceTexture::Absent => None,
            FaceTexture::Untextured => Some(None),
            FaceTexture::Index(i) if i < self.textures.len() => Some(Some(i)),
            FaceTexture::Index(i) => {
                self.warnings.push(format!(
                    "{what}: texture index {i} out of range, drawn untextured"
                ));
                Some(None)
            }
        }
    }

    fn uv_space(&self, texture: Option<usize>) -> [f32; 2] {
        texture
            .and_then(|t| self.uv_space.get(t).copied())
            .unwrap_or(self.default_uv_space)
    }

    fn cube(&mut self, c: &Cube, node: usize) {
        // Blockbench's adjustFromAndToForInflateAndStretch.
        let mut from = [0.0; 3];
        let mut to = [0.0; 3];
        for i in 0..3 {
            let half = (c.to[i] - c.from[i]) / 2.0;
            let centre = c.from[i] + half;
            from[i] = centre - (half + c.inflate) * c.stretch[i];
            to[i] = centre + (half + c.inflate) * c.stretch[i];
        }
        let (f, t) = (sub(from, c.origin), sub(to, c.origin));
        let place = self.placer(node, c.origin, c.rotation);
        let q = euler_zyx_quat(c.rotation);
        let box_uv = c.box_uv.unwrap_or(self.file.meta.box_uv);
        let box_uvs = box_uv.then(|| box_uv_rects(c));

        // setShape's vertex order per face, and its outward normal.
        let faces: [(&str, [[f32; 3]; 4], [f32; 3]); 6] = [
            (
                "east",
                [
                    [t[0], t[1], t[2]],
                    [t[0], t[1], f[2]],
                    [t[0], f[1], t[2]],
                    [t[0], f[1], f[2]],
                ],
                [1.0, 0.0, 0.0],
            ),
            (
                "west",
                [
                    [f[0], t[1], f[2]],
                    [f[0], t[1], t[2]],
                    [f[0], f[1], f[2]],
                    [f[0], f[1], t[2]],
                ],
                [-1.0, 0.0, 0.0],
            ),
            (
                "up",
                [
                    [f[0], t[1], f[2]],
                    [t[0], t[1], f[2]],
                    [f[0], t[1], t[2]],
                    [t[0], t[1], t[2]],
                ],
                [0.0, 1.0, 0.0],
            ),
            (
                "down",
                [
                    [f[0], f[1], t[2]],
                    [t[0], f[1], t[2]],
                    [f[0], f[1], f[2]],
                    [t[0], f[1], f[2]],
                ],
                [0.0, -1.0, 0.0],
            ),
            (
                "south",
                [
                    [f[0], t[1], t[2]],
                    [t[0], t[1], t[2]],
                    [f[0], f[1], t[2]],
                    [t[0], f[1], t[2]],
                ],
                [0.0, 0.0, 1.0],
            ),
            (
                "north",
                [
                    [t[0], t[1], f[2]],
                    [f[0], t[1], f[2]],
                    [t[0], f[1], f[2]],
                    [f[0], f[1], f[2]],
                ],
                [0.0, 0.0, -1.0],
            ),
        ];
        for (dir, corners, normal) in faces {
            let Some(face) = c.faces.get(dir) else {
                continue;
            };
            let what = format!("cube {:?} {dir}", c.name);
            let Some(texture) = self.texture_for(&what, &face_texture(face.get("texture"))) else {
                continue;
            };
            let mut uv = match box_uvs.as_ref().and_then(|b| b.get(dir)) {
                Some(rect) => *rect,
                None => {
                    let mut r = [0.0; 4];
                    if let Some(Value::Array(a)) = face.get("uv") {
                        for (o, x) in r.iter_mut().zip(a) {
                            *o = x.as_f64().unwrap_or(0.0) as f32;
                        }
                    }
                    r
                }
            };
            if box_uv {
                // updateUV's anti-bleed margin.
                for si in 0..2 {
                    let margin = if uv[si] > uv[si + 2] {
                        -1.0 / 64.0
                    } else {
                        1.0 / 64.0
                    };
                    uv[si] += margin;
                    uv[si + 2] -= margin;
                }
            }
            let [w, h] = self.uv_space(texture);
            let mut arr = [
                [uv[0] / w, uv[1] / h],
                [uv[2] / w, uv[1] / h],
                [uv[0] / w, uv[3] / h],
                [uv[2] / w, uv[3] / h],
            ];
            let mut rot = face.get("rotation").and_then(Value::as_f64).unwrap_or(0.0) as i32;
            while rot > 0 {
                let a = arr[0];
                arr[0] = arr[2];
                arr[2] = arr[3];
                arr[3] = arr[1];
                arr[1] = a;
                rot -= 90;
            }
            let n = quat_rotate(q, normal);
            let part = self.part(node, texture);
            let base: Vec<u32> = (0..4)
                .map(|i| part.push_vertex(place(corners[i]), n, arr[i]))
                .collect();
            // three.js index order (0,2,1)(2,3,1): counter-clockwise from outside.
            part.indices
                .extend([base[0], base[2], base[1], base[2], base[3], base[1]]);
        }
    }

    fn mesh(&mut self, m: &MeshElement, node: usize) {
        let place = self.placer(node, m.origin, m.rotation);
        let mut keys: Vec<&String> = m.faces.keys().collect();
        keys.sort();
        for key in keys {
            let face = &m.faces[key];
            if face.vertices.len() < 3 {
                continue;
            }
            let what = format!("mesh {:?} face {key}", m.name);
            let Some(texture) = self.texture_for(&what, &face_texture(Some(&face.texture))) else {
                continue;
            };
            let Some(pos) = face
                .vertices
                .iter()
                .map(|k| m.vertices.get(k).copied())
                .collect::<Option<Vec<_>>>()
            else {
                self.warnings
                    .push(format!("{what}: unknown vertex key, skipped"));
                continue;
            };
            let order: Vec<usize> = if pos.len() == 4 {
                sorted_quad(&pos)
            } else {
                (0..pos.len()).collect()
            };
            if pos.len() > 4 {
                self.warnings
                    .push(format!("{what}: {} vertices, fanned", pos.len()));
            }
            let p = |i: usize| pos[order[i]];
            let normal = normalize(cross(sub(p(1), p(0)), sub(p(2), p(0))));
            let n = quat_rotate(euler_zyx_quat(m.rotation), normal);
            let [w, h] = self.uv_space(texture);
            let uv_of = |i: usize| {
                let k = &face.vertices[order[i]];
                let uv = face.uv.get(k).copied().unwrap_or([0.0, 0.0]);
                [uv[0] / w, uv[1] / h]
            };
            let corners: Vec<([f32; 3], [f32; 2])> =
                (0..pos.len()).map(|i| (place(p(i)), uv_of(i))).collect();
            let part = self.part(node, texture);
            let base: Vec<u32> = corners
                .iter()
                .map(|(pp, uv)| part.push_vertex(*pp, n, *uv))
                .collect();
            for i in 1..base.len() - 1 {
                part.indices.extend([base[0], base[i], base[i + 1]]);
            }
        }
    }
}

/// Box-UV face rectangles `[x1, y1, x2, y2]` (Blockbench `updateUV`).
fn box_uv_rects(c: &Cube) -> HashMap<&'static str, [f32; 4]> {
    // Cube.size(undefined, floored): box UV uses whole pixels.
    let size: Vec<f32> = (0..3)
        .map(|i| (c.to[i] - c.from[i] + 1e-7).floor())
        .collect();
    let (x, y, z) = (size[0], size[1], size[2]);
    let mut list = [
        ("east", [0.0, z], [z, y]),
        ("west", [z + x, z], [z, y]),
        ("up", [z + x, z], [-x, -z]),
        ("down", [z + x * 2.0, 0.0], [-x, z]),
        ("south", [z * 2.0 + x, z], [x, y]),
        ("north", [z, z], [x, y]),
    ];
    if c.mirror_uv {
        for f in list.iter_mut() {
            f.1[0] += f.2[0];
            f.2[0] *= -1.0;
        }
        let (east, west) = (list[0], list[1]);
        list[0] = ("east", west.1, west.2);
        list[1] = ("west", east.1, east.2);
    }
    list.iter()
        .map(|(name, from, size)| {
            let [ox, oy] = c.uv_offset;
            (
                *name,
                [
                    from[0] + ox,
                    from[1] + oy,
                    from[0] + size[0] + ox,
                    from[1] + size[1] + oy,
                ],
            )
        })
        .collect()
}

/// Blockbench `MeshFace.getSortedVertices` for a quad: the order that walks
/// its outline rather than crossing it.
fn sorted_quad(p: &[[f32; 3]]) -> Vec<usize> {
    // Whether `check` is on the far side of line base1-base2 from `top`.
    let test = |b1: [f32; 3], b2: [f32; 3], top: [f32; 3], check: [f32; 3]| {
        let d = sub(b2, b1);
        let len2 = dot(d, d);
        let t = if len2 > 0.0 {
            dot(sub(top, b1), d) / len2
        } else {
            0.0
        };
        let closest = add(b1, scale(d, t));
        let normal = sub(closest, top);
        dot(normal, sub(check, b2)) > 0.0
    };
    if test(p[1], p[2], p[0], p[3]) {
        vec![2, 0, 1, 3]
    } else if test(p[0], p[1], p[2], p[3]) {
        vec![0, 2, 1, 3]
    } else {
        vec![0, 1, 2, 3]
    }
}

fn placeholder_texture(t: &TextureEntry) -> Texture {
    Texture {
        name: t.name.clone(),
        width: 0,
        height: 0,
        rgba: Vec::new(),
        alpha: AlphaKind::Opaque,
    }
}

#[cfg(not(feature = "textures"))]
fn decode_texture(
    t: &TextureEntry,
    _resolve: &mut dyn FnMut(&str) -> Option<Vec<u8>>,
) -> Result<Texture, Error> {
    Err(Error::Texture {
        name: t.name.clone(),
        reason: "built without the `textures` feature (use parse_rig)".into(),
    })
}

#[cfg(feature = "textures")]
fn decode_texture(
    t: &TextureEntry,
    resolve: &mut dyn FnMut(&str) -> Option<Vec<u8>>,
) -> Result<Texture, Error> {
    let err = |reason: String| Error::Texture {
        name: t.name.clone(),
        reason,
    };
    let bytes = match t.source.as_deref().and_then(|s| s.strip_prefix("data:")) {
        Some(data) => {
            let (_, b64) = data
                .split_once(";base64,")
                .ok_or_else(|| err("source is not base64".into()))?;
            base64::engine::general_purpose::STANDARD
                .decode(b64.trim())
                .map_err(|e| err(e.to_string()))?
        }
        None => {
            let path = t
                .relative_path
                .as_deref()
                .or(t.path.as_deref())
                .unwrap_or_default();
            resolve(path)
                .ok_or_else(|| err(format!("not embedded and {path:?} could not be read")))?
        }
    };
    let img = image::load_from_memory(&bytes)
        .map_err(|e| err(e.to_string()))?
        .to_rgba8();
    let (width, height) = img.dimensions();
    let rgba = img.into_raw();
    let mut partial = false;
    let mut clear = false;
    for a in rgba.iter().skip(3).step_by(4) {
        match *a {
            255 => {}
            0 => clear = true,
            _ => partial = true,
        }
    }
    let alpha = if partial {
        AlphaKind::Blend
    } else if clear {
        AlphaKind::Mask
    } else {
        AlphaKind::Opaque
    };
    Ok(Texture {
        name: t.name.clone(),
        width,
        height,
        rgba,
        alpha,
    })
}

// --- Math -----------------------------------------------------------------------

fn add(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn scale(a: [f32; 3], s: f32) -> [f32; 3] {
    [a[0] * s, a[1] * s, a[2] * s]
}
fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn normalize(a: [f32; 3]) -> [f32; 3] {
    let l = dot(a, a).sqrt();
    if l > 0.0 { scale(a, 1.0 / l) } else { a }
}

/// `[x, y, z, w]` of `Rz * Ry * Rx` (three.js Euler order `ZYX`), degrees in.
pub fn euler_zyx_quat(deg: [f32; 3]) -> [f32; 4] {
    let axis = |i: usize| {
        // libm, not std: the simulation evaluates rigs, and std's sin/cos
        // differ between native and wasm32.
        let h = deg[i].to_radians() / 2.0;
        let mut q = [0.0, 0.0, 0.0, libm::cosf(h)];
        q[i] = libm::sinf(h);
        q
    };
    quat_mul(quat_mul(axis(2), axis(1)), axis(0))
}

pub fn quat_mul(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    let [ax, ay, az, aw] = a;
    let [bx, by, bz, bw] = b;
    [
        aw * bx + ax * bw + ay * bz - az * by,
        aw * by - ax * bz + ay * bw + az * bx,
        aw * bz + ax * by - ay * bx + az * bw,
        aw * bw - ax * bx - ay * by - az * bz,
    ]
}

pub fn quat_rotate(q: [f32; 4], v: [f32; 3]) -> [f32; 3] {
    let u = [q[0], q[1], q[2]];
    let t = scale(cross(u, v), 2.0);
    add(add(v, scale(t, q[3])), cross(u, t))
}

impl Model {
    /// Model-space position of each node's origin and its accumulated rotation.
    pub fn world_transforms(&self) -> Vec<([f32; 3], [f32; 4])> {
        let mut out: Vec<([f32; 3], [f32; 4])> = Vec::with_capacity(self.nodes.len());
        for node in &self.nodes {
            let local = (node.translation, node.rotation_quat());
            out.push(match node.parent {
                None => local,
                Some(p) => {
                    let (pt, pr) = out[p];
                    (add(pt, quat_rotate(pr, local.0)), quat_mul(pr, local.1))
                }
            });
        }
        out
    }

    /// Axis-aligned bounds of every vertex, model space (world units).
    pub fn bounds(&self) -> Option<([f32; 3], [f32; 3])> {
        let world = self.world_transforms();
        let mut b: Option<([f32; 3], [f32; 3])> = None;
        for (node, (t, r)) in self.nodes.iter().zip(&world) {
            for part in &node.parts {
                for p in &part.positions {
                    let w = add(*t, quat_rotate(*r, *p));
                    let (lo, hi) = b.get_or_insert((w, w));
                    for i in 0..3 {
                        lo[i] = lo[i].min(w[i]);
                        hi[i] = hi[i].max(w[i]);
                    }
                }
            }
        }
        b
    }

    pub fn animation(&self, name: &str) -> Option<&Animation> {
        self.animations.iter().find(|a| a.name == name)
    }

    /// Every node's local transform `(translation, rotation [x,y,z,w], scale)`
    /// with `poses` (from [`Animation::sample`]) applied: rotation offsets are
    /// added to the rest Euler angles (Blockbench adds them per component),
    /// positions added, scales multiplied. Nodes without a pose keep rest.
    pub fn local_pose(&self, poses: &[(usize, BonePose)]) -> Vec<([f32; 3], [f32; 4], [f32; 3])> {
        let mut out: Vec<_> = self
            .nodes
            .iter()
            .map(|n| (n.translation, n.rotation_quat(), [1.0f32; 3]))
            .collect();
        for (node, pose) in poses {
            let Some(n) = self.nodes.get(*node) else {
                continue;
            };
            // Groups of bone-less formats carry no transform to animate.
            let euler = [0, 1, 2].map(|i| n.rotation_deg[i] + pose.rotation[i]);
            out[*node] = (
                add(n.translation, pose.position),
                euler_zyx_quat(euler),
                pose.scale,
            );
        }
        out
    }

    pub fn triangle_count(&self) -> usize {
        self.nodes
            .iter()
            .flat_map(|n| &n.parts)
            .map(Part::triangle_count)
            .sum()
    }
}
