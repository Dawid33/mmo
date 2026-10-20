//! Blockbench models in Bevy: a `.bbmodel` asset loader on top of the
//! engine-free `bbmodel` crate, and a component that spawns one.
//!
//! ```ignore
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
//! Textures are sampled nearest (pixel art) and clamped; each texture's alpha
//! mode comes from its pixels (`bbmodel::AlphaKind`). Faces are drawn from
//! both sides, because Blockbench does and modellers rely on it (planes for
//! ears, leaves, wings).
use bevy::asset::io::Reader;
use bevy::asset::{AssetLoader, LoadContext, RenderAssetUsages};
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
    /// Animation names in the file (not played yet).
    pub animations: Vec<String>,
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

/// Set on a `BbModel` entity once its children exist.
#[derive(Component)]
pub struct BbModelSpawned;

pub struct BbModelPlugin;

impl Plugin for BbModelPlugin {
    fn build(&self, app: &mut App) {
        app.init_asset::<BbModelAsset>()
            .register_asset_loader(BbModelLoader)
            .add_systems(Update, (respawn_reloaded_bbmodels, spawn_bbmodels).chain());
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
        reader.read_to_end(&mut bytes).await.map_err(|e| BbModelLoadError(e.to_string()))?;
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
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(bytes) else { return Vec::new() };
    v.get("textures")
        .and_then(|t| t.as_array())
        .into_iter()
        .flatten()
        .filter(|t| t.get("source").and_then(|s| s.as_str()).is_none_or(|s| !s.starts_with("data:")))
        .filter_map(|t| t.get("relative_path").or_else(|| t.get("path")).and_then(|p| p.as_str()))
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
    let untextured = cx.add_labeled_asset("MaterialUntextured".to_string(), material(None, bbmodel::AlphaKind::Opaque));
    let bounds = model.bounds().map(|(lo, hi)| (Vec3::from(lo), Vec3::from(hi)));
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
                    let material = part.texture.and_then(|t| materials.get(t).cloned()).unwrap_or(untextured.clone());
                    (mesh, material)
                })
                .collect(),
        })
        .collect();
    BbModelAsset { name: model.name, nodes, bounds, animations: model.animations }
}

fn texture_image(t: &bbmodel::Texture) -> Image {
    let mut image = Image::new(
        Extent3d { width: t.width, height: t.height, depth_or_array_layers: 1 },
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
        base_color: if texture.is_some() { Color::WHITE } else { Color::srgb(0.6, 0.6, 0.6) },
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
    Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::RENDER_WORLD)
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, part.positions.clone())
        .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, part.normals.clone())
        .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, part.uvs.clone())
        .with_inserted_indices(Indices::U32(part.indices.clone()))
}

/// Builds the child hierarchy of every `BbModel` whose asset has loaded.
pub fn spawn_bbmodels(
    mut commands: Commands,
    models: Res<Assets<BbModelAsset>>,
    pending: Query<(Entity, &BbModel), Without<BbModelSpawned>>,
) {
    for (entity, model) in &pending {
        let Some(asset) = models.get(&model.0) else { continue };
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
                commands.spawn((
                    Mesh3d(mesh.clone()),
                    MeshMaterial3d(material.clone()),
                    Transform::IDENTITY,
                    BbModelPart,
                    ChildOf(id),
                ));
            }
            spawned.push(id);
        }
        commands.entity(entity).insert(BbModelSpawned);
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
        let AssetEvent::Modified { id } = event else { continue };
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Headless: the asset server loads real Blockbench files (the bbmodel
    /// crate's MIT fixtures) and `spawn_bbmodels` builds the hierarchy.
    fn app() -> App {
        let mut app = App::new();
        app.add_plugins((
            MinimalPlugins,
            AssetPlugin { file_path: "../bbmodel/tests/fixtures".into(), ..default() },
            bevy::transform::TransformPlugin,
        ))
        .init_asset::<Mesh>()
        .init_asset::<Image>()
        .init_asset::<StandardMaterial>()
        .add_plugins(BbModelPlugin);
        app
    }

    fn spawn_and_wait(app: &mut App, path: &'static str) -> Entity {
        let handle: Handle<BbModelAsset> = app.world().resource::<AssetServer>().load(path);
        let entity = app.world_mut().spawn(BbModel(handle)).id();
        for _ in 0..400 {
            app.update();
            if app.world().entity(entity).contains::<BbModelSpawned>() {
                return entity;
            }
            if let Some(bevy::asset::LoadState::Failed(e)) =
                app.world().resource::<AssetServer>().get_load_state(&app.world().get::<BbModel>(entity).unwrap().0)
            {
                panic!("{path}: {e}");
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("{path} never spawned");
    }

    #[test]
    fn loads_and_spawns_a_real_model() {
        let mut app = app();
        let e = spawn_and_wait(&mut app, "hephaestus/dragon.bbmodel");
        app.update();
        let world = app.world_mut();
        let meshes: usize = world.query_filtered::<(), With<Mesh3d>>().iter(world).count();
        let nodes: usize = world.query_filtered::<(), (With<BbModelPart>, Without<Mesh3d>)>().iter(world).count();
        let handle = world.get::<BbModel>(e).unwrap().0.clone();
        let asset = world.resource::<Assets<BbModelAsset>>().get(&handle).unwrap();
        assert_eq!(nodes, asset.nodes.len(), "one entity per node");
        assert_eq!(meshes, asset.nodes.iter().map(|n| n.parts.len()).sum::<usize>());
        assert!(meshes > 10, "the dragon has many parts: {meshes}");
        let (lo, hi) = asset.bounds.unwrap();
        assert!(hi.y - lo.y > 16.0, "taller than a block: {lo} {hi}");
        // Every mesh and material is resolvable.
        for node in &asset.nodes {
            for (mesh, material) in &node.parts {
                assert!(world.resource::<Assets<Mesh>>().get(mesh).is_some());
                assert!(world.resource::<Assets<StandardMaterial>>().get(material).is_some());
            }
        }
    }

    #[test]
    fn a_file_that_is_not_a_bbmodel_fails_to_load() {
        let mut app = app();
        let handle: Handle<BbModelAsset> = app.world().resource::<AssetServer>().load("hephaestus/LICENSE.bbmodel");
        for _ in 0..200 {
            app.update();
            if let Some(bevy::asset::LoadState::Failed(_)) = app.world().resource::<AssetServer>().get_load_state(&handle) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("missing file should fail");
    }
}
