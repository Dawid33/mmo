//! Blockbench models in Bevy: a `.bbmodel` asset loader on top of the
//! engine-free `bbmodel` crate, and a component that spawns one.
//!
//! ```text
//! let model: Handle<BbModelAsset> = asset_server.load("models/soldier.bbmodel");
//! commands.spawn((BbModel(model), Transform::from_xyz(x, y, z)));
//! ```
//!
//! `spawn_bbmodels` gives the entity a child per Blockbench group (named after
//! it, posed at its pivot) holding one `Mesh3d` per texture. The model keeps
//! Blockbench's units — one pixel is one world unit, 16 to a block — so a
//! model drawn at block scale needs no scaling. Editing the file while the
//! client runs reloads it in place (Bevy's asset hot reload, native only).
//!
//! Animations play through [`BbAnimationPlayer`] on the same entity:
//!
//! ```text
//! commands.spawn((BbModel(model), BbAnimationPlayer::playing("walk")));
//! player.crossfade("idle", 0.2); // later, from a system
//! ```
//!
//! `animate_bbmodels` samples the clip each frame exactly as Blockbench plays
//! it (`bbmodel::animation`) and writes every group entity's `Transform`.
//! `once` clips stop on their last frame and report [`BbAnimationPlayer::finished`],
//! so gameplay can chain them (fall -> land -> standup -> idle).
//!
//! Textures are sampled nearest (pixel art) and clamped; each texture's alpha
//! mode comes from its pixels (`bbmodel::AlphaKind`). Faces are drawn from
//! both sides, because Blockbench does and modellers rely on it (planes for
//! ears, leaves, wings).
use bevy::asset::io::Reader;
use bevy::asset::{AssetLoader, LoadContext, RenderAssetUsages};
use bevy::camera::visibility::RenderLayers;
use bevy::image::{ImageAddressMode, ImageSampler, ImageSamplerDescriptor};
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};

/// A loaded Blockbench model: its node tree with ready meshes and materials.
#[derive(Asset, TypePath, Debug)]
pub struct BbModelAsset {
    pub name: String,
    /// `nodes[0]` is the root; a parent always precedes its children.
    pub nodes: Vec<BbModelNode>,
    /// Model-space bounds, world units (min, max).
    pub bounds: Option<(Vec3, Vec3)>,
    /// The rest skeleton and the animations, for `animate_bbmodels` (the
    /// geometry and textures live in the labeled assets instead).
    pub skeleton: bbmodel::Model,
}

impl BbModelAsset {
    pub fn animation_names(&self) -> impl Iterator<Item = &str> {
        self.skeleton.animations.iter().map(|a| a.name.as_str())
    }
}

#[derive(Debug)]
pub struct BbModelNode {
    pub name: String,
    pub parent: Option<usize>,
    pub transform: Transform,
    pub parts: Vec<(Handle<Mesh>, Handle<StandardMaterial>)>,
}

/// Spawn this model under the entity (which needs a `Transform`).
#[derive(Component, Clone, Debug)]
#[require(Transform, Visibility)]
pub struct BbModel(pub Handle<BbModelAsset>);

/// Marks the entities `spawn_bbmodels` made for a model, so a reload can
/// replace them.
#[derive(Component)]
pub struct BbModelPart;

/// Set on a `BbModel` entity once its children exist: the entity of every
/// node, in `BbModelAsset::nodes` order.
#[derive(Component)]
pub struct BbModelSpawned(pub Vec<Entity>);

/// Plays one of the model's animations. Put it on the `BbModel` entity.
#[derive(Component, Debug, Clone)]
pub struct BbAnimationPlayer {
    /// The clip playing, `None` for the rest pose.
    pub animation: Option<String>,
    /// Seconds since the clip started (scaled by `speed`).
    pub elapsed: f32,
    pub speed: f32,
    pub paused: bool,
    /// Molang inputs (`variable.*` etc.) the clips may read.
    pub variables: bbmodel::molang::Context,
    /// The clip being faded out: (name, its elapsed time, fade length, fade progress).
    fade: Option<(Option<String>, f32, f32, f32)>,
}

impl Default for BbAnimationPlayer {
    fn default() -> Self {
        Self {
            animation: None,
            elapsed: 0.0,
            speed: 1.0,
            paused: false,
            variables: Default::default(),
            fade: None,
        }
    }
}

impl BbAnimationPlayer {
    pub fn playing(name: &str) -> Self {
        Self {
            animation: Some(name.to_string()),
            ..Self::default()
        }
    }

    /// Switch to `name` from its start, at once.
    pub fn play(&mut self, name: &str) {
        self.animation = Some(name.to_string());
        self.elapsed = 0.0;
        self.fade = None;
    }

    /// Switch to `name` from its start, blending from the current pose over
    /// `seconds`.
    pub fn crossfade(&mut self, name: &str, seconds: f32) {
        self.fade = Some((self.animation.take(), self.elapsed, seconds.max(1e-3), 0.0));
        self.animation = Some(name.to_string());
        self.elapsed = 0.0;
    }

    /// Back to the rest pose.
    pub fn stop(&mut self) {
        self.animation = None;
        self.elapsed = 0.0;
        self.fade = None;
    }

    /// Whether the current `once` clip has played out (always false for
    /// `loop` / `hold` clips and for no clip).
    pub fn finished(&self, model: &BbModelAsset) -> bool {
        self.animation
            .as_deref()
            .and_then(|n| model.skeleton.animation(n))
            .is_some_and(|a| a.finished(self.elapsed))
    }
}

pub struct BbModelPlugin;

impl Plugin for BbModelPlugin {
    fn build(&self, app: &mut App) {
        app.init_asset::<BbModelAsset>()
            .register_asset_loader(BbModelLoader)
            .add_systems(
                Update,
                (respawn_reloaded_bbmodels, spawn_bbmodels, animate_bbmodels).chain(),
            );
    }
}

#[derive(Default, TypePath)]
pub struct BbModelLoader;

#[derive(Debug)]
pub struct BbModelLoadError(pub String);

impl std::fmt::Display for BbModelLoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for BbModelLoadError {}

impl AssetLoader for BbModelLoader {
    type Asset = BbModelAsset;
    type Settings = ();
    type Error = BbModelLoadError;

    async fn load(
        &self,
        reader: &mut dyn Reader,
        _settings: &(),
        load_context: &mut LoadContext<'_>,
    ) -> Result<BbModelAsset, BbModelLoadError> {
        let mut bytes = Vec::new();
        reader
            .read_to_end(&mut bytes)
            .await
            .map_err(|e| BbModelLoadError(e.to_string()))?;
        // Textures that are not embedded are files next to the model: read
        // them up front, since the parser's resolver is synchronous.
        let mut external = std::collections::HashMap::new();
        for path in external_texture_paths(&bytes) {
            if let Ok(asset_path) = load_context.path().resolve_embed_str(&path) {
                if let Ok(data) = load_context.read_asset_bytes(asset_path).await {
                    external.insert(path, data);
                }
            }
        }
        let model = bbmodel::parse_with(&bytes, &mut |p| external.get(p).cloned())
            .map_err(|e| BbModelLoadError(format!("{}: {e}", load_context.path())))?;
        for w in &model.warnings {
            debug!("bbmodel {}: {w}", load_context.path());
        }
        Ok(build_asset(model, load_context))
    }

    fn extensions(&self) -> &[&str] {
        &["bbmodel"]
    }
}

/// `relative_path` / `path` of every texture with no embedded source.
fn external_texture_paths(bytes: &[u8]) -> Vec<String> {
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return Vec::new();
    };
    v.get("textures")
        .and_then(|t| t.as_array())
        .into_iter()
        .flatten()
        .filter(|t| {
            t.get("source")
                .and_then(|s| s.as_str())
                .is_none_or(|s| !s.starts_with("data:"))
        })
        .filter_map(|t| {
            t.get("relative_path")
                .or_else(|| t.get("path"))
                .and_then(|p| p.as_str())
        })
        .map(str::to_owned)
        .collect()
}

fn build_asset(model: bbmodel::Model, cx: &mut LoadContext<'_>) -> BbModelAsset {
    let materials: Vec<Handle<StandardMaterial>> = model
        .textures
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let image = cx.add_labeled_asset(format!("Texture{i}"), texture_image(t));
            cx.add_labeled_asset(format!("Material{i}"), material(Some(image), t.alpha))
        })
        .collect();
    let untextured = cx.add_labeled_asset(
        "MaterialUntextured".to_string(),
        material(None, bbmodel::AlphaKind::Opaque),
    );
    let bounds = model
        .bounds()
        .map(|(lo, hi)| (Vec3::from(lo), Vec3::from(hi)));
    for a in &model.animations {
        debug!(
            "bbmodel {}: animation {:?} {:?} {:.2}s",
            cx.path(),
            a.name,
            a.loop_mode,
            a.length
        );
    }
    let nodes = model
        .nodes
        .iter()
        .enumerate()
        .map(|(n, node)| BbModelNode {
            name: node.name.clone(),
            parent: node.parent,
            transform: Transform::from_translation(Vec3::from(node.translation))
                .with_rotation(Quat::from_array(node.rotation_quat())),
            parts: node
                .parts
                .iter()
                .enumerate()
                .map(|(p, part)| {
                    let mesh = cx.add_labeled_asset(format!("Mesh{n}_{p}"), part_mesh(part));
                    let material = part
                        .texture
                        .and_then(|t| materials.get(t).cloned())
                        .unwrap_or(untextured.clone());
                    (mesh, material)
                })
                .collect(),
        })
        .collect();
    let mut skeleton = model;
    skeleton.textures.clear();
    for node in &mut skeleton.nodes {
        node.parts.clear();
    }
    BbModelAsset {
        name: skeleton.name.clone(),
        nodes,
        bounds,
        skeleton,
    }
}

fn texture_image(t: &bbmodel::Texture) -> Image {
    let mut image = Image::new(
        Extent3d {
            width: t.width,
            height: t.height,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        t.rgba.clone(),
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    );
    image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::ClampToEdge,
        address_mode_v: ImageAddressMode::ClampToEdge,
        ..ImageSamplerDescriptor::nearest()
    });
    image
}

fn material(texture: Option<Handle<Image>>, alpha: bbmodel::AlphaKind) -> StandardMaterial {
    StandardMaterial {
        base_color: if texture.is_some() {
            Color::WHITE
        } else {
            Color::srgb(0.6, 0.6, 0.6)
        },
        base_color_texture: texture,
        alpha_mode: match alpha {
            bbmodel::AlphaKind::Opaque => AlphaMode::Opaque,
            bbmodel::AlphaKind::Mask => AlphaMode::Mask(0.5),
            bbmodel::AlphaKind::Blend => AlphaMode::Blend,
        },
        double_sided: true,
        cull_mode: None,
        perceptual_roughness: 1.0,
        metallic: 0.0,
        reflectance: 0.1,
        ..default()
    }
}

fn part_mesh(part: &bbmodel::Part) -> Mesh {
    Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::RENDER_WORLD,
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, part.positions.clone())
    .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, part.normals.clone())
    .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, part.uvs.clone())
    .with_inserted_indices(Indices::U32(part.indices.clone()))
}

/// Builds the child hierarchy of every `BbModel` whose asset has loaded.
pub fn spawn_bbmodels(
    mut commands: Commands,
    models: Res<Assets<BbModelAsset>>,
    pending: Query<(Entity, &BbModel, Option<&RenderLayers>), Without<BbModelSpawned>>,
) {
    for (entity, model, layers) in &pending {
        let Some(asset) = models.get(&model.0) else {
            continue;
        };
        let mut spawned: Vec<Entity> = Vec::with_capacity(asset.nodes.len());
        for node in &asset.nodes {
            let parent = node.parent.map_or(entity, |p| spawned[p]);
            let id = commands
                .spawn((
                    Name::new(node.name.clone()),
                    node.transform,
                    Visibility::default(),
                    BbModelPart,
                    ChildOf(parent),
                ))
                .id();
            for (mesh, material) in &node.parts {
                let part = commands
                    .spawn((
                        Mesh3d(mesh.clone()),
                        MeshMaterial3d(material.clone()),
                        Transform::IDENTITY,
                        BbModelPart,
                        ChildOf(id),
                    ))
                    .id();
                // The model's render layers are its meshes' (they are not
                // inherited): a model on a layer of its own stays on it.
                if let Some(layers) = layers {
                    commands.entity(part).insert(layers.clone());
                }
            }
            spawned.push(id);
        }
        commands.entity(entity).insert(BbModelSpawned(spawned));
    }
}

/// Poses every animated model's group entities for this frame.
pub fn animate_bbmodels(
    time: Res<Time>,
    models: Res<Assets<BbModelAsset>>,
    mut players: Query<(&BbModel, &BbModelSpawned, &mut BbAnimationPlayer)>,
    mut transforms: Query<&mut Transform, With<BbModelPart>>,
) {
    let dt = time.delta_secs();
    for (model, spawned, mut player) in &mut players {
        let Some(asset) = models.get(&model.0) else {
            continue;
        };
        let skeleton = &asset.skeleton;
        if !player.paused {
            let step = dt * player.speed;
            player.elapsed += step;
            if let Some(fade) = player.fade.as_mut() {
                fade.1 += step;
                fade.3 += dt;
            }
        }
        let sample = |name: &Option<String>, elapsed: f32| -> Vec<(usize, bbmodel::BonePose)> {
            name.as_deref()
                .and_then(|n| skeleton.animation(n))
                .map(|a| a.sample(a.local_time(elapsed), &player.variables))
                .unwrap_or_default()
        };
        let mut poses = sample(&player.animation, player.elapsed);
        if let Some((from, from_elapsed, length, progress)) = player.fade.clone() {
            let w = (progress / length).clamp(0.0, 1.0);
            if w >= 1.0 {
                player.fade = None;
            } else {
                poses = bbmodel::animation::blend(&sample(&from, from_elapsed), &poses, w);
            }
        }
        for (i, (t, r, s)) in skeleton.local_pose(&poses).into_iter().enumerate() {
            let Some(&e) = spawned.0.get(i) else { continue };
            if let Ok(mut tf) = transforms.get_mut(e) {
                tf.translation = Vec3::from(t);
                tf.rotation = Quat::from_array(r);
                tf.scale = Vec3::from(s);
            }
        }
    }
}

/// A model file changed on disk (hot reload): drop its spawned parts so
/// `spawn_bbmodels` builds them again from the new asset.
pub fn respawn_reloaded_bbmodels(
    mut commands: Commands,
    mut events: MessageReader<AssetEvent<BbModelAsset>>,
    spawned: Query<(Entity, &BbModel, &Children), With<BbModelSpawned>>,
    parts: Query<(), With<BbModelPart>>,
) {
    for event in events.read() {
        let AssetEvent::Modified { id } = event else {
            continue;
        };
        for (entity, model, children) in &spawned {
            if model.0.id() != *id {
                continue;
            }
            for child in children.iter() {
                if parts.contains(child) {
                    commands.entity(child).despawn();
                }
            }
            commands.entity(entity).remove::<BbModelSpawned>();
        }
    }
}
