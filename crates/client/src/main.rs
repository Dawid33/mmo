#![allow(unused)]
//! Game client
// #![deny(missing_docs)]
use bevy::{
    asset::RenderAssetUsages,
    color::palettes::css::WHITE,
    diagnostic::{FrameTimeDiagnosticsPlugin, LogDiagnosticsPlugin},
    ecs::system::SystemState,
    input::{
        keyboard::KeyboardInput,
        mouse::{AccumulatedMouseMotion, MouseButtonInput, MouseMotion},
    },
    log::{Level, LogPlugin},
    pbr::{
        wireframe::{WireframeConfig, WireframePlugin},
        CascadeShadowConfigBuilder,
    },
    platform::collections::HashMap,
    prelude::*,
    render::{
        mesh::{Indices, PrimitiveTopology, VertexAttributeValues},
        settings::{PowerPreference, RenderCreation, WgpuFeatures, WgpuSettings},
        view::NoIndirectDrawing,
        RenderPlugin,
    },
    window::{CursorGrabMode, PrimaryWindow},
};
use block_mesh::RIGHT_HANDED_Y_UP_CONFIG;
use crossbeam::{
    channel::{Receiver, Sender},
    select,
};
use game::{
    BevyEvent, ClientPacket, ClientUpdateEvent, EntityKey, GameDataTransactionKind, GameDataUpdate,
    GameError, GameEvent, GameEventKind, PlayerKey, Region, INDUCED_LATENCY,
};
use iyes_perf_ui::{prelude::PerfUiAllEntries, PerfUiPlugin};
use log::{info, trace, warn, LevelFilter};
use ndshape::ConstShape;
use noise::NoiseFn;
use pyroscope::PyroscopeAgent;
use pyroscope_pprofrs::{pprof_backend, PprofConfig};
use rapier3d::{math::Isometry, na::Perspective3, parry::simba::scalar::SupersetOf};
use simplelog::{FormatItem, SimpleLogger};
use std::{
    collections::BTreeMap,
    net::{Ipv4Addr, SocketAddr, SocketAddrV4},
    ops::Deref,
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

mod layout;
mod netcode;
mod render_world;
mod state;
mod text;
mod voxel;
mod window;

/// Wrapper struct for coordinating networking / rollback for the game.
pub struct GameInstanceManager {
    game_event_send: Sender<GameEventKind>,
    game_event_recv: Receiver<GameEventKind>,
    client_event_send: Sender<ClientUpdateEvent>,
    server: SocketAddr,
}

impl GameInstanceManager {
    /// Create new GameInstanceManager
    ///
    /// - Talk to ingress to figure out which regions to load for a given player.
    /// - Start listening game events and log them.
    /// - Download game data
    /// - Re-simulate game based on logged events.
    /// - Send spawn player event and enter main loop
    pub fn new(
        game_event_send: Sender<GameEventKind>,
        game_event_recv: Receiver<GameEventKind>,
        client_event_send: Sender<ClientUpdateEvent>,
        server: SocketAddr,
    ) -> Self {
        Self {
            game_event_recv,
            game_event_send,
            client_event_send,
            server,
        }
    }

    /// Recieve and process game events from the client and network, in that order.
    ///
    /// - If received a client event:
    ///   - Simulate the event and push it on to a buffer.
    ///   - Send it to the players authoritative region.
    ///   - The client buffer should always be larger than the
    ///     network buffer by at least the number of ticks the client is ahead of
    ///     the server.
    /// - If recieved a network event:
    ///   - Check if the network and client event on the front of each buffer match.
    ///   - Keep doing this until the network buffer is empty. Reconcile the region
    ///     each time the comparison succeeds.
    ///   - Any time it doesn't succeed, rollback the client, apply the network
    ///     event and then re-apply the client events that were just rolled back.
    ///
    /// If sync clock event: send client update to window with desired input delay
    /// and update tick time.
    pub fn connect_and_run(&mut self) -> Result<(), GameError> {
        let tick_sender = self.game_event_send.clone();
        let tick_rate = Arc::new(AtomicU64::new(game::TICK_RATE));
        let tick_thread_tick_rate = tick_rate.clone();
        // Generate ticks
        std::thread::spawn(move || loop {
            // TODO: Sync ticks with server.
            tick_sender.send(GameEventKind::Tick).unwrap();
            let rate = tick_thread_tick_rate.load(Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(rate));
        });

        let (server_send, server_recv) = crossbeam::channel::unbounded();
        let (server_game_send, server_game_recv) = crossbeam::channel::unbounded();
        let mut conn = netcode::ServerConnection::new(server_send, server_game_recv, self.server);
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async { conn.connect_and_handle().await.unwrap() });
        });

        server_game_send
            .send(game::ClientPacket::RequestRegion)
            .unwrap();

        let mut world: Option<game::World> = None;
        let mut now = Instant::now();
        let mut ready = false;
        loop {
            select! {
                recv(server_recv) -> server_msg => {
                    match server_msg.unwrap() {
                        game::ServerPacket::SyncClock(region_id, server_tick_rate, server_tick, rtt) => {
                            if let Some(ref mut world) = world {
                                let client_tick = world.current_tick(&region_id);
                                let diff: isize = client_tick as isize - server_tick as isize;
                                // this is how far behind the server is
                                let milisecond_diff = diff * server_tick_rate as isize;
                                // we subract the rtt to get a more accurate approximation of how far behind the server is.
                                let total_mili_diff = milisecond_diff - rtt.as_millis() as isize;

                                if total_mili_diff < INDUCED_LATENCY {
                                    ready = false;
                                    tick_rate.store((server_tick_rate as f32 * 0.2) as u64, Ordering::SeqCst);
                                } else if total_mili_diff > INDUCED_LATENCY {
                                    ready = true;
                                    tick_rate.store((server_tick_rate as f32 * 1.2) as u64, Ordering::SeqCst);
                                } else {
                                    ready = true;
                                    tick_rate.store(server_tick_rate, Ordering::SeqCst);
                                }
                            }
                        },
                        // TODO: buffer incoming game events until region / world is loaded, then handle all at once
                        // and enable client game events.
                        game::ServerPacket::GameEvent(game_event) => {
                            // info!("{:?}", now.elapsed());
                            // now = Instant::now();
                            if let Some(ref mut world) = world {
                                match world.reconcile_event(game_event) {
                                    Ok(_) => (),
                                    Err(e) => {warn!("{:?}", e); return Err(GameError::CrashedOnServerEvent)},
                                };
                            }
                        }
                        game::ServerPacket::Region(id, mut raw_game_data, last_id, key) => {
                            self.client_event_send.send(ClientUpdateEvent::SetPlayer(key));
                            let (send, recv) = crossbeam::channel::unbounded();
                            let mut data = Region::new(raw_game_data.clone(), Some(send), id);
                            self.client_event_send.send(ClientUpdateEvent::NewRegion(id, (*raw_game_data.data).clone(), recv)).unwrap();
                            let mut w = game::World::new();
                            w.load(&id, data, last_id);
                            world = Some(w);
                            info!("Region recieved and loaded!");
                        }
                    }
                },
                recv(self.game_event_recv) -> game_event => {
                    if let Some(ref mut world) = world {
                         match game_event.clone() {
                            Ok(event) => {
                                match event {
                                    GameEventKind::Quit => return Ok(()),
                                    e => {
                                        if let GameEventKind::PlayerBevyEvent(_,_) = e {
                                            // don't handle player events until sim has caught up with server.
                                            if !ready {
                                                continue;
                                            }
                                        }
                                        let event = world.handle_event(game_event.unwrap(), 0)?;
                                        server_game_send.send(game::ClientPacket::GameEvent(event)).unwrap();
                                    }
                                }
                            },
                            Err(e) => panic!("{}", e),
                        }
                    }
                }
            }
        }
    }
}

/// Event sent from client to game thread.
pub enum Command {
    /// Connect to a server, sync and start running game sim.
    ConnectToServerAndScene(
        Sender<GameEventKind>,
        Receiver<GameEventKind>,
        Sender<ClientUpdateEvent>,
        SocketAddr,
    ),
    /// Quit the game thread. Should only be send when quitting the application.
    Quit,
}

fn start_game_thread() -> Sender<Command> {
    let (command_send, command_recv) = crossbeam::channel::unbounded();
    std::thread::spawn(move || loop {
        match command_recv.recv() {
            Ok(command) => match command {
                Command::ConnectToServerAndScene(sender, receiver, client_sender, server) => {
                    let mut manager =
                        GameInstanceManager::new(sender, receiver, client_sender, server);
                    if let Err(e) = manager.connect_and_run() {
                        warn!("Game Crashed: {:?}", e);
                    };
                }
                Command::Quit => {
                    trace!("Game thread recieved quit command.");
                    break;
                }
            },
            Err(_e) => {
                warn!(
                    "Game thread stoped receiving command events, stopping game thread. Client probably crashed or was closed incorrectly."
                );
                break;
            }
        }
    });
    return command_send;
}

use bevy::app::{plugin_group, Plugin};

use crate::voxel::{
    chunk::{PaddedChunkShape, VoxelArray, CHUNK_SIZE_U},
    plugin::VoxelWorldPlugin,
    prelude::{
        ChunkMeshingDelegate, TextureIndexMapperFn, VoxelLookupDelegate, VoxelWorldCamera,
        VoxelWorldConfig,
    },
    rendering::ATTRIBUTE_TEX_INDEX,
    voxel::WorldVoxel,
};

plugin_group! {
    /// This plugin group will add all the default plugins for a *Bevy* application:
    pub struct MyPlugins {
        bevy::app:::TaskPoolPlugin,
        bevy::diagnostic:::FrameCountPlugin,
        bevy::time:::TimePlugin,
        bevy::app:::ScheduleRunnerPlugin,
        bevy::winit:::WinitPlugin,
    }
    /// [`DefaultPlugins`] obeys *Cargo* *feature* flags. Users may exert control over this plugin group
    /// by disabling `default-features` in their `Cargo.toml` and enabling only those features
    /// that they wish to use.
    ///
    /// [`DefaultPlugins`] contains all the plugins typically required to build
    /// a *Bevy* application which includes a *window* and presentation components.
    /// For the absolute minimum number of plugins needed to run a Bevy application, see [`MinimalPlugins`].
}

const FORMAT: &'static [FormatItem] = &[FormatItem::Literal("client".as_bytes())];

#[derive(Resource, Deref)]
pub struct CommandSender(Sender<Command>);

#[derive(Resource, Deref)]
pub struct GameEventSender(Sender<GameEventKind>);

#[derive(Resource, Deref)]
pub struct ClientUpdateReceiver(Receiver<ClientUpdateEvent>);

#[derive(Resource, Deref)]
pub struct Player(Option<PlayerKey>);

#[derive(Resource, Default)]
pub struct State {
    regions: BTreeMap<usize, RegionHandle>,
    entity_map: BTreeMap<EntityKey, Entity>,
}

#[derive(Debug, Component, Clone, Copy, PartialEq, Default, Deref, DerefMut)]
struct PhysicalTranslation(Transform);

#[derive(Debug, Component, Clone, Copy, PartialEq, Default, Deref, DerefMut)]
struct PreviousPhysicalTranslation(Transform);

#[derive(Component)]
pub struct RegionId(usize);

#[derive(Debug, Component, Clone, Deref, DerefMut)]
pub struct EntityKeyComponent(EntityKey);

#[derive(Debug, Component)]
struct WorldModelCamera;

pub struct RegionHandle {
    receiver: Receiver<GameDataUpdate>,
    player_key: PlayerKey,
}

pub fn setup(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    commands.spawn(PerfUiAllEntries::default());
    let cascade_shadow_config = CascadeShadowConfigBuilder { ..default() }.build();
    commands.spawn((
        DirectionalLight {
            color: Color::srgb(0.98, 0.95, 0.82),
            shadows_enabled: true,
            ..default()
        },
        Transform::from_xyz(0.0, 0.0, 0.0).looking_at(Vec3::new(-0.15, -0.1, 0.15), Vec3::Y),
        cascade_shadow_config,
    ));

    commands.insert_resource(AmbientLight {
        color: Color::srgb(0.98, 0.95, 0.82),
        brightness: 100.0,
        affects_lightmapped_meshes: true,
    });
}

fn handle_input(
    mut keyboard_input_events: EventReader<KeyboardInput>,
    mut mouse_button_input_events: EventReader<MouseButtonInput>,
    mut window: Query<(&mut Window), With<PrimaryWindow>>,
    sender: Res<GameEventSender>,
    player: Res<Player>,
) {
    let p = if let Some(p) = (*player).as_ref() {
        p
    } else {
        return;
    };

    for event in keyboard_input_events.read() {
        sender.send(GameEventKind::PlayerBevyEvent(
            *p,
            BevyEvent::KeyboardInput(event.clone()),
        ));
    }

    for event in mouse_button_input_events.read() {
        sender.send(GameEventKind::PlayerBevyEvent(
            *p,
            BevyEvent::MouseButtonInput(event.clone()),
        ));
    }

    let mut window = window.single_mut().unwrap();
    if let CursorGrabMode::Locked = window.cursor_options.grab_mode {
        let center = Vec2::new(window.width() / 2.0, window.height() / 2.0);
        window.set_cursor_position(Some(center));
    }
}

/// Update bevy representation of game state.
pub fn update_client(
    mut commands: Commands,
    client: Res<ClientUpdateReceiver>,
    sender: Res<GameEventSender>,
    mut state: ResMut<State>,
    mut player: ResMut<Player>,
    mut mouse_motion: Res<AccumulatedMouseMotion>,
) {
    while let Ok(event) = client.try_recv() {
        match event {
            ClientUpdateEvent::NewRegion(id, game, receiver) => {
                let player_key = player.expect(
                    "Player key should exit by the time game data is being generated in client.",
                );
                let player_entity = game.players.get(player_key).unwrap();
                for (e, _) in game.ecs.entities.iter() {
                    if &e == player_entity {
                        if let Some(camera) = game.ecs.camera.try_get(e) {
                            let b = game
                                .physics
                                .bodies
                                .get(camera.view_matrix.unwrap())
                                .unwrap();
                            info!("spawning camera");
                            commands.spawn((
                                WorldModelCamera,
                                Camera3d::default(),
                                NoIndirectDrawing::default(),
                                Projection::from(PerspectiveProjection {
                                    fov: camera.proj_matrix.fovy(),
                                    aspect_ratio: camera.proj_matrix.aspect(),
                                    near: camera.proj_matrix.znear(),
                                    far: camera.proj_matrix.zfar(),
                                }),
                                Transform::from_xyz(
                                    b.translation().x,
                                    b.translation().y,
                                    b.translation().z,
                                ),
                                PhysicalTranslation::default(),
                                PreviousPhysicalTranslation::default(),
                                EntityKeyComponent(e),
                                VoxelWorldCamera::<MainWorld>::default(),
                            ));
                        }
                    }
                }
                state.regions.insert(
                    id,
                    RegionHandle {
                        receiver,
                        player_key,
                    },
                );
            }
            ClientUpdateEvent::GameCrash(game_error) => panic!("{:?}", game_error),
            ClientUpdateEvent::SetPlayer(key) => {
                player.0.insert(key);
            }
        }
    }
    if let Some(player) = **player {
        sender.send(GameEventKind::PlayerBevyEvent(
            player,
            BevyEvent::MouseMotionInput(mouse_motion.delta),
        ));
    }
}

fn add_new_entites_to_map(
    mut commands: Commands,
    mut state: ResMut<State>,
    query: Query<(Entity, &EntityKeyComponent), Added<EntityKeyComponent>>,
) {
    for (entity, key) in &query {
        state.entity_map.insert(**key, entity);
    }
}

pub fn update_regions(world: &mut World) {
    let mut system_state: SystemState<(
        Res<ClientUpdateReceiver>,
        Commands,
        ResMut<State>,
        ResMut<Player>,
    )> = SystemState::new(world);
    let mut updates = BTreeMap::new();
    let (client, commands, state, player) = system_state.get_mut(world);
    for (i, handle) in state.regions.iter() {
        while let Ok(event) = handle.receiver.try_recv() {
            if !updates.contains_key(i) {
                updates.insert(*i, Vec::new());
            }
            match event.update_kind {
                game::GameDataUpdateKind::CreateUIElement(_, _, _)
                | game::GameDataUpdateKind::SetUIElementStyle(_, _)
                | game::GameDataUpdateKind::SetUIElementContent(_, _)
                | game::GameDataUpdateKind::RemoveUIElement(_) => (),
                game::GameDataUpdateKind::SetVoxelComponent(entity_key, _)
                | game::GameDataUpdateKind::SetEntityPosition(entity_key, _)
                | game::GameDataUpdateKind::UpdateCameraViewProj(entity_key, _)
                | game::GameDataUpdateKind::UpdateCameraViewMatrix(entity_key, _)
                | game::GameDataUpdateKind::SetFreeCam(entity_key, _)
                | game::GameDataUpdateKind::CreateEntity(entity_key)
                | game::GameDataUpdateKind::RemoveEntity(entity_key) => {
                    let e = state.entity_map.get(&entity_key).unwrap();
                    updates.get_mut(i).unwrap().push((*e, event));
                }
            }
        }
    }

    for (region_id, list) in updates {
        for (entity, event) in list {
            if let GameDataTransactionKind::Undo = event.do_kind {
                continue;
            }
            match event.update_kind {
                game::GameDataUpdateKind::UpdateCameraViewProj(entity_key, perspective3) => {}
                game::GameDataUpdateKind::UpdateCameraViewMatrix(entity_key, isometry) => {
                    let mut e = world.entity_mut(entity);
                    let mut c = e.get::<PhysicalTranslation>().unwrap().clone();
                    let mut previous = e.get_mut::<PreviousPhysicalTranslation>().unwrap();
                    **previous = *c;
                    let mut c = e.get_mut::<PhysicalTranslation>().unwrap();
                    c.translation.x = isometry.translation.x;
                    c.translation.y = isometry.translation.y;
                    c.translation.z = isometry.translation.z;
                    c.rotation.x = isometry.rotation.i;
                    c.rotation.y = isometry.rotation.j;
                    c.rotation.z = isometry.rotation.k;
                    c.rotation.w = isometry.rotation.w;
                }
                game::GameDataUpdateKind::SetVoxelComponent(entity_key, entity) => {
                    info!("voxel data recieved: {:?}", entity);
                }
                // game::GameDataUpdateKind::CreateEntity(entity_key) => todo!(),
                // game::GameDataUpdateKind::RemoveEntity(entity_key) => todo!(),
                game::GameDataUpdateKind::SetFreeCam(entity_key, value) => {
                    // TODO: Only change the free cam mode if it applies to this clients player
                    let mut window = world
                        .query_filtered::<(&mut Window), With<PrimaryWindow>>()
                        .single_mut(world)
                        .unwrap();
                    if value {
                        window.cursor_options.grab_mode = CursorGrabMode::Locked;
                        window.cursor_options.visible = false;
                    } else {
                        window.cursor_options.grab_mode = CursorGrabMode::None;
                        window.cursor_options.visible = true;
                    }
                }
                _ => (),
            }
        }
    }
}

fn lerp_physical_position(
    fixed_time: Res<Time<Fixed>>,
    mut query: Query<(
        &mut Transform,
        &PhysicalTranslation,
        &PreviousPhysicalTranslation,
    )>,
) {
    for (mut transform, current_physical_translation, previous_physical_translation) in
        query.iter_mut()
    {
        // let previous = previous_physical_translation.0;
        let current = current_physical_translation.0;
        // The overstep fraction is a value between 0 and 1 that tells us how far we are between two fixed timesteps.
        let alpha = fixed_time.overstep_fraction();

        let rendered_translation = transform.translation.lerp(current.translation, 0.1);
        let rendered_rotation = transform.rotation.slerp(current.rotation, 0.1);
        transform.translation = rendered_translation;
        transform.rotation = rendered_rotation;
    }
}

#[derive(Resource, Clone, Default)]
struct MainWorld;

impl VoxelWorldConfig for MainWorld {
    type MaterialIndex = u8;
    type ChunkUserBundle = ();

    fn spawning_distance(&self) -> u32 {
        10
    }

    fn min_despawn_distance(&self) -> u32 {
        1
    }

    fn voxel_lookup_delegate(&self) -> VoxelLookupDelegate<Self::MaterialIndex> {
        Box::new(move |_chunk_pos| get_voxel_fn())
    }

    fn texture_index_mapper(&self) -> Arc<dyn Fn(Self::MaterialIndex) -> [u32; 3] + Send + Sync> {
        Arc::new(|mat| match mat {
            0 => [0, 0, 0],
            1 => [1, 1, 1],
            2 => [2, 2, 2],
            3 => [3, 3, 3],
            _ => [0, 0, 0],
        })
    }

    // A custom meshing delegate can be added via the config implementation
    //
    // In this example we use the greedy meshing algorithm from the block_mesh crate
    // instead of the default simple meshing.
    //
    // The closure returned here is executed on a thread in the task pool, so it's OK to block
    // for as long as needed.
    fn chunk_meshing_delegate(
        &self,
    ) -> ChunkMeshingDelegate<Self::MaterialIndex, Self::ChunkUserBundle> {
        Some(Box::new(|pos: IVec3| {
            // If necessary, we can caputure data here based on the chunk position
            // and move it into the closure below.
            Box::new(
                // The array of voxels for the chunk
                |voxels: Arc<VoxelArray<Self::MaterialIndex>>,
                 // A reference to the texture index mapper function as defined in the config
                 texture_index_mapper: TextureIndexMapperFn<Self::MaterialIndex>| {
                    let faces = block_mesh::RIGHT_HANDED_Y_UP_CONFIG.faces;
                    let mut buffer = block_mesh::GreedyQuadsBuffer::new(voxels.len());

                    // Call the greedy meshing algorithm from the block_mesh crate
                    block_mesh::greedy_quads(
                        &*voxels,
                        &PaddedChunkShape {},
                        [0; 3],
                        [CHUNK_SIZE_U + 1; 3],
                        &faces,
                        &mut buffer,
                    );

                    let num_indices = buffer.quads.num_quads() * 6;
                    let num_vertices = buffer.quads.num_quads() * 4;
                    let mut indices = Vec::with_capacity(num_indices);
                    let mut positions = Vec::with_capacity(num_vertices);
                    let mut normals = Vec::with_capacity(num_vertices);
                    let mut tex_coords = Vec::with_capacity(num_vertices);
                    let mut material_types = Vec::with_capacity(num_vertices);

                    for (group, face) in buffer.quads.groups.into_iter().zip(faces.into_iter()) {
                        for quad in group.into_iter() {
                            let normal = IVec3::from([
                                face.signed_normal().x,
                                face.signed_normal().y,
                                face.signed_normal().z,
                            ]);

                            indices
                                .extend_from_slice(&face.quad_mesh_indices(positions.len() as u32));
                            positions.extend_from_slice(&face.quad_mesh_positions(&quad, 1.0));
                            normals.extend_from_slice(&face.quad_mesh_normals());

                            tex_coords.extend_from_slice(&face.tex_coords(
                                RIGHT_HANDED_Y_UP_CONFIG.u_flip_face,
                                true,
                                &quad.into(),
                            ));

                            let voxel_index = PaddedChunkShape::linearize(quad.minimum) as usize;
                            let material_type = match voxels[voxel_index] {
                                // Here we call the texture index mapper function to get the texture index
                                // for the material type of the voxel
                                WorldVoxel::Solid(mt) => texture_index_mapper(mt),
                                _ => [1, 1, 1],
                            };
                            material_types.extend(std::iter::repeat(material_type).take(4));
                        }
                    }

                    let mut render_mesh = Mesh::new(
                        PrimitiveTopology::TriangleList,
                        RenderAssetUsages::RENDER_WORLD,
                    );
                    render_mesh.insert_attribute(
                        Mesh::ATTRIBUTE_POSITION,
                        VertexAttributeValues::Float32x3(positions),
                    );
                    render_mesh.insert_attribute(
                        Mesh::ATTRIBUTE_NORMAL,
                        VertexAttributeValues::Float32x3(normals),
                    );
                    render_mesh.insert_attribute(
                        Mesh::ATTRIBUTE_UV_0,
                        VertexAttributeValues::Float32x2(vec![[0.0; 2]; num_vertices]),
                    );
                    render_mesh.insert_attribute(
                        ATTRIBUTE_TEX_INDEX,
                        VertexAttributeValues::Uint32x3(material_types),
                    );
                    render_mesh
                        .insert_attribute(Mesh::ATTRIBUTE_COLOR, vec![[1.0; 4]; num_vertices]);
                    render_mesh.insert_indices(Indices::U32(indices.clone()));

                    // The second value in this tuple is an optional component bundle.
                    // If you want to generate some custom data for the chunk, like a nav mesh,
                    // you can put it here in a regular Bevy component. This will then get added
                    // to the spawned Chunk entity.
                    // The type of this bundle is defined in the `ChunkUserBundle` associated type.
                    (render_mesh, None)
                },
            )
        }))
    }
}

fn main() {
    let agent = PyroscopeAgent::builder("http://localhost:4040", "rust-app")
        .backend(pprof_backend(PprofConfig::new().sample_rate(100)))
        .tags(vec![("kind", "client")])
        .build()
        .unwrap();

    let agent_running = agent.start().unwrap();

    let sender = start_game_thread();
    let (game_send, game_recv) = crossbeam::channel::unbounded();
    let (client_send, client_recv) = crossbeam::channel::unbounded();
    sender
        .send(Command::ConnectToServerAndScene(
            game_send.clone(),
            game_recv,
            client_send,
            SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 1), 6466)),
        ))
        .unwrap();

    App::new()
        .add_plugins((
            DefaultPlugins
                .set(RenderPlugin {
                    render_creation: RenderCreation::Automatic(WgpuSettings {
                        power_preference: PowerPreference::LowPower,
                        features: WgpuFeatures::POLYGON_MODE_LINE,
                        ..default()
                    }),
                    ..default()
                })
                .set(LogPlugin {
                    filter: "client=trace,warn".to_string(),
                    level: Level::TRACE,
                    ..Default::default()
                }),
            WireframePlugin::default(),
        ))
        .add_plugins(VoxelWorldPlugin::with_config(MainWorld))
        .add_plugins(FrameTimeDiagnosticsPlugin::default())
        .add_plugins(PerfUiPlugin)
        // .insert_resource(WireframeConfig {
        //     global: true,
        //     default_color: WHITE.into(),
        // })
        .insert_resource(CommandSender(sender))
        .insert_resource(GameEventSender(game_send))
        .insert_resource(ClientUpdateReceiver(client_recv))
        .insert_resource(State::default())
        .insert_resource(Player(None))
        .add_systems(Startup, setup)
        .add_systems(Update, handle_input)
        .add_systems(RunFixedMainLoop, {
            (
                update_client.before(add_new_entites_to_map),
                add_new_entites_to_map
                    .after(update_client)
                    .before(update_regions),
                update_regions.after(add_new_entites_to_map),
                lerp_physical_position.after(update_regions),
            )
        })
        .run();
    let agent_ready = agent_running.stop().unwrap();
    agent_ready.shutdown();
}

fn get_voxel_fn() -> Box<dyn FnMut(IVec3) -> WorldVoxel + Send + Sync> {
    // Set up some noise to use as the terrain height map
    let mut noise = noise::HybridMulti::<noise::Perlin>::new(1234);
    noise.octaves = 5;
    noise.frequency = 1.1;
    noise.lacunarity = 2.8;
    noise.persistence = 0.4;

    // We use this to cache the noise value for each y column so we only need
    // to calculate it once per x/z coordinate
    let mut cache = HashMap::<(i32, i32), f64>::new();

    // Then we return this boxed closure that captures the noise and the cache
    // This will get sent off to a separate thread for meshing by bevy_voxel_world
    Box::new(move |pos: IVec3| {
        // Sea level
        if pos.y < 1 {
            return WorldVoxel::Solid(3);
        }

        let [x, y, z] = pos.as_dvec3().to_array();

        // If y is less than the noise sample, we will set the voxel to solid
        let is_ground = y < match cache.get(&(pos.x, pos.z)) {
            Some(sample) => *sample,
            None => {
                let sample = noise.get([x / 4000.0, z / 4000.0]) * 20.0;
                cache.insert((pos.x, pos.z), sample);
                sample
            }
        };

        if is_ground {
            // Solid voxel of material type 0
            WorldVoxel::Solid(0)
        } else {
            WorldVoxel::Air
        }
    })
}
