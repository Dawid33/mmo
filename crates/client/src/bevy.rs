// #[derive(Resource, Deref)]
// pub struct CommandSender(Sender<Command>);

// #[derive(Resource, Deref)]
// pub struct GameEventSender(Sender<GameEventKind>);

// #[derive(Resource, Deref)]
// pub struct ClientUpdateReceiver(Receiver<ClientUpdateEvent>);

// #[derive(Resource, Deref)]
// pub struct Player(Option<PlayerKey>);

// #[derive(Resource, Default)]
// pub struct State {
//     regions: BTreeMap<usize, RegionHandle>,
//     entity_map: BTreeMap<EntityKey, Entity>,
// }

// #[derive(Debug, Component, Clone, Copy, PartialEq, Default, Deref, DerefMut)]
// struct PhysicalTranslation(Transform);

// #[derive(Debug, Component, Clone, Copy, PartialEq, Default, Deref, DerefMut)]
// struct PreviousPhysicalTranslation(Transform);

// #[derive(Component)]
// pub struct RegionId(usize);

// #[derive(Debug, Component, Clone, Deref, DerefMut)]
// pub struct EntityKeyComponent(EntityKey);

// #[derive(Debug, Component)]
// struct WorldModelCamera;

// pub struct RegionHandle {
//     receiver: Receiver<GameDataUpdate>,
//     player_key: PlayerKey,
// }

// pub fn setup(
//     mut commands: Commands,
//     mut meshes: ResMut<Assets<Mesh>>,
//     mut materials: ResMut<Assets<StandardMaterial>>,
// ) {
//     commands.spawn(PerfUiAllEntries::default());
//     let cascade_shadow_config = CascadeShadowConfigBuilder { ..default() }.build();
//     commands.spawn((
//         DirectionalLight {
//             color: Color::srgb(0.98, 0.95, 0.82),
//             shadows_enabled: true,
//             ..default()
//         },
//         Transform::from_xyz(0.0, 0.0, 0.0).looking_at(Vec3::new(-0.15, -0.1, 0.15), Vec3::Y),
//         cascade_shadow_config,
//     ));

//     commands.insert_resource(AmbientLight {
//         color: Color::srgb(0.98, 0.95, 0.82),
//         brightness: 100.0,
//         affects_lightmapped_meshes: true,
//     });
// }

// fn handle_input(
//     mut keyboard_input_events: EventReader<KeyboardInput>,
//     mut mouse_button_input_events: EventReader<MouseButtonInput>,
//     mut window: Query<(&mut Window), With<PrimaryWindow>>,
//     sender: Res<GameEventSender>,
//     player: Res<Player>,
// ) {
//     let p = if let Some(p) = (*player).as_ref() {
//         p
//     } else {
//         return;
//     };

//     for event in keyboard_input_events.read() {
//         sender.send(GameEventKind::PlayerBevyEvent(
//             *p,
//             BevyEvent::KeyboardInput(event.clone()),
//         ));
//     }

//     for event in mouse_button_input_events.read() {
//         sender.send(GameEventKind::PlayerBevyEvent(
//             *p,
//             BevyEvent::MouseButtonInput(event.clone()),
//         ));
//     }

//     let mut window = window.single_mut().unwrap();
//     if let CursorGrabMode::Locked = window.cursor_options.grab_mode {
//         let center = Vec2::new(window.width() / 2.0, window.height() / 2.0);
//         window.set_cursor_position(Some(center));
//     }
// }

// /// Update bevy representation of game state.
// pub fn update_client(
//     mut commands: Commands,
//     client: Res<ClientUpdateReceiver>,
//     sender: Res<GameEventSender>,
//     mut state: ResMut<State>,
//     mut player: ResMut<Player>,
//     mut mouse_motion: Res<AccumulatedMouseMotion>,
// ) {
//     while let Ok(event) = client.try_recv() {
//         match event {
//             ClientUpdateEvent::NewRegion(id, game, receiver) => {
//                 let player_key = player.expect(
//                     "Player key should exit by the time game data is being generated in client.",
//                 );
//                 let player_entity = game.players.get(player_key).unwrap();
//                 for (e, _) in game.ecs.entities.iter() {
//                     if &e == player_entity {
//                         if let Some(camera) = game.ecs.camera.try_get(e) {
//                             let b = game
//                                 .physics
//                                 .bodies
//                                 .get(camera.view_matrix.unwrap())
//                                 .unwrap();
//                             info!("spawning camera");
//                             commands.spawn((
//                                 WorldModelCamera,
//                                 Camera3d::default(),
//                                 NoIndirectDrawing::default(),
//                                 Projection::from(PerspectiveProjection {
//                                     fov: camera.proj_matrix.fovy(),
//                                     aspect_ratio: camera.proj_matrix.aspect(),
//                                     near: camera.proj_matrix.znear(),
//                                     far: camera.proj_matrix.zfar(),
//                                 }),
//                                 Transform::from_xyz(
//                                     b.translation().x,
//                                     b.translation().y,
//                                     b.translation().z,
//                                 ),
//                                 PhysicalTranslation::default(),
//                                 PreviousPhysicalTranslation::default(),
//                                 EntityKeyComponent(e),
//                                 VoxelWorldCamera::<MainWorld>::default(),
//                             ));
//                         }
//                     }
//                 }
//                 state.regions.insert(
//                     id,
//                     RegionHandle {
//                         receiver,
//                         player_key,
//                     },
//                 );
//             }
//             ClientUpdateEvent::GameCrash(game_error) => panic!("{:?}", game_error),
//             ClientUpdateEvent::SetPlayer(key) => {
//                 player.0.insert(key);
//             }
//         }
//     }
//     if let Some(player) = **player {
//         sender.send(GameEventKind::PlayerBevyEvent(
//             player,
//             BevyEvent::MouseMotionInput(mouse_motion.delta),
//         ));
//     }
// }

// fn add_new_entites_to_map(
//     mut commands: Commands,
//     mut state: ResMut<State>,
//     query: Query<(Entity, &EntityKeyComponent), Added<EntityKeyComponent>>,
// ) {
//     for (entity, key) in &query {
//         state.entity_map.insert(**key, entity);
//     }
// }

// pub fn update_regions(world: &mut World) {
//     let mut system_state: SystemState<(
//         Res<ClientUpdateReceiver>,
//         Commands,
//         ResMut<State>,
//         ResMut<Player>,
//     )> = SystemState::new(world);
//     let mut updates = BTreeMap::new();
//     let (client, commands, state, player) = system_state.get_mut(world);
//     for (i, handle) in state.regions.iter() {
//         while let Ok(event) = handle.receiver.try_recv() {
//             if !updates.contains_key(i) {
//                 updates.insert(*i, Vec::new());
//             }
//             match event.update_kind {
//                 game::GameDataUpdateKind::CreateUIElement(_, _, _)
//                 | game::GameDataUpdateKind::SetUIElementStyle(_, _)
//                 | game::GameDataUpdateKind::SetUIElementContent(_, _)
//                 | game::GameDataUpdateKind::RemoveUIElement(_) => (),
//                 game::GameDataUpdateKind::SetVoxelComponent(entity_key, _)
//                 | game::GameDataUpdateKind::SetEntityPosition(entity_key, _)
//                 | game::GameDataUpdateKind::UpdateCameraViewProj(entity_key, _)
//                 | game::GameDataUpdateKind::UpdateCameraViewMatrix(entity_key, _)
//                 | game::GameDataUpdateKind::SetFreeCam(entity_key, _)
//                 | game::GameDataUpdateKind::CreateEntity(entity_key)
//                 | game::GameDataUpdateKind::RemoveEntity(entity_key) => {
//                     let e = state.entity_map.get(&entity_key).unwrap();
//                     updates.get_mut(i).unwrap().push((*e, event));
//                 }
//             }
//         }
//     }

//     for (region_id, list) in updates {
//         for (entity, event) in list {
//             if let GameDataTransactionKind::Undo = event.do_kind {
//                 continue;
//             }
//             match event.update_kind {
//                 game::GameDataUpdateKind::UpdateCameraViewProj(entity_key, perspective3) => {}
//                 game::GameDataUpdateKind::UpdateCameraViewMatrix(entity_key, isometry) => {
//                     let mut e = world.entity_mut(entity);
//                     let mut c = e.get::<PhysicalTranslation>().unwrap().clone();
//                     let mut previous = e.get_mut::<PreviousPhysicalTranslation>().unwrap();
//                     **previous = *c;
//                     let mut c = e.get_mut::<PhysicalTranslation>().unwrap();
//                     c.translation.x = isometry.translation.x;
//                     c.translation.y = isometry.translation.y;
//                     c.translation.z = isometry.translation.z;
//                     c.rotation.x = isometry.rotation.i;
//                     c.rotation.y = isometry.rotation.j;
//                     c.rotation.z = isometry.rotation.k;
//                     c.rotation.w = isometry.rotation.w;
//                 }
//                 game::GameDataUpdateKind::SetVoxelComponent(entity_key, entity) => {
//                     info!("voxel data recieved: {:?}", entity);
//                 }
//                 // game::GameDataUpdateKind::CreateEntity(entity_key) => todo!(),
//                 // game::GameDataUpdateKind::RemoveEntity(entity_key) => todo!(),
//                 game::GameDataUpdateKind::SetFreeCam(entity_key, value) => {
//                     // TODO: Only change the free cam mode if it applies to this clients player
//                     let mut window = world
//                         .query_filtered::<(&mut Window), With<PrimaryWindow>>()
//                         .single_mut(world)
//                         .unwrap();
//                     if value {
//                         window.cursor_options.grab_mode = CursorGrabMode::Locked;
//                         window.cursor_options.visible = false;
//                     } else {
//                         window.cursor_options.grab_mode = CursorGrabMode::None;
//                         window.cursor_options.visible = true;
//                     }
//                 }
//                 _ => (),
//             }
//         }
//     }
// }

// fn lerp_physical_position(
//     fixed_time: Res<Time<Fixed>>,
//     mut query: Query<(
//         &mut Transform,
//         &PhysicalTranslation,
//         &PreviousPhysicalTranslation,
//     )>,
// ) {
//     for (mut transform, current_physical_translation, previous_physical_translation) in
//         query.iter_mut()
//     {
//         // let previous = previous_physical_translation.0;
//         let current = current_physical_translation.0;
//         // The overstep fraction is a value between 0 and 1 that tells us how far we are between two fixed timesteps.
//         let alpha = fixed_time.overstep_fraction();

//         let rendered_translation = transform.translation.lerp(current.translation, 0.1);
//         let rendered_rotation = transform.rotation.slerp(current.rotation, 0.1);
//         transform.translation = rendered_translation;
//         transform.rotation = rendered_rotation;
//     }
// }

// #[derive(Resource, Clone, Default)]
// struct MainWorld;

// impl VoxelWorldConfig for MainWorld {
//     type MaterialIndex = u8;
//     type ChunkUserBundle = ();

//     fn spawning_distance(&self) -> u32 {
//         10
//     }

//     fn min_despawn_distance(&self) -> u32 {
//         1
//     }

//     fn voxel_lookup_delegate(&self) -> VoxelLookupDelegate<Self::MaterialIndex> {
//         Box::new(move |_chunk_pos| get_voxel_fn())
//     }

//     fn texture_index_mapper(&self) -> Arc<dyn Fn(Self::MaterialIndex) -> [u32; 3] + Send + Sync> {
//         Arc::new(|mat| match mat {
//             0 => [0, 0, 0],
//             1 => [1, 1, 1],
//             2 => [2, 2, 2],
//             3 => [3, 3, 3],
//             _ => [0, 0, 0],
//         })
//     }

//     // A custom meshing delegate can be added via the config implementation
//     //
//     // In this example we use the greedy meshing algorithm from the block_mesh crate
//     // instead of the default simple meshing.
//     //
//     // The closure returned here is executed on a thread in the task pool, so it's OK to block
//     // for as long as needed.
//     fn chunk_meshing_delegate(
//         &self,
//     ) -> ChunkMeshingDelegate<Self::MaterialIndex, Self::ChunkUserBundle> {
//         Some(Box::new(|pos: IVec3| {
//             // If necessary, we can caputure data here based on the chunk position
//             // and move it into the closure below.
//             Box::new(
//                 // The array of voxels for the chunk
//                 |voxels: Arc<VoxelArray<Self::MaterialIndex>>,
//                  // A reference to the texture index mapper function as defined in the config
//                  texture_index_mapper: TextureIndexMapperFn<Self::MaterialIndex>| {
//                     let faces = block_mesh::RIGHT_HANDED_Y_UP_CONFIG.faces;
//                     let mut buffer = block_mesh::GreedyQuadsBuffer::new(voxels.len());

//                     // Call the greedy meshing algorithm from the block_mesh crate
//                     block_mesh::greedy_quads(
//                         &*voxels,
//                         &PaddedChunkShape {},
//                         [0; 3],
//                         [CHUNK_SIZE_U + 1; 3],
//                         &faces,
//                         &mut buffer,
//                     );

//                     let num_indices = buffer.quads.num_quads() * 6;
//                     let num_vertices = buffer.quads.num_quads() * 4;
//                     let mut indices = Vec::with_capacity(num_indices);
//                     let mut positions = Vec::with_capacity(num_vertices);
//                     let mut normals = Vec::with_capacity(num_vertices);
//                     let mut tex_coords = Vec::with_capacity(num_vertices);
//                     let mut material_types = Vec::with_capacity(num_vertices);

//                     for (group, face) in buffer.quads.groups.into_iter().zip(faces.into_iter()) {
//                         for quad in group.into_iter() {
//                             let normal = IVec3::from([
//                                 face.signed_normal().x,
//                                 face.signed_normal().y,
//                                 face.signed_normal().z,
//                             ]);

//                             indices
//                                 .extend_from_slice(&face.quad_mesh_indices(positions.len() as u32));
//                             positions.extend_from_slice(&face.quad_mesh_positions(&quad, 1.0));
//                             normals.extend_from_slice(&face.quad_mesh_normals());

//                             tex_coords.extend_from_slice(&face.tex_coords(
//                                 RIGHT_HANDED_Y_UP_CONFIG.u_flip_face,
//                                 true,
//                                 &quad.into(),
//                             ));

//                             let voxel_index = PaddedChunkShape::linearize(quad.minimum) as usize;
//                             let material_type = match voxels[voxel_index] {
//                                 // Here we call the texture index mapper function to get the texture index
//                                 // for the material type of the voxel
//                                 WorldVoxel::Solid(mt) => texture_index_mapper(mt),
//                                 _ => [1, 1, 1],
//                             };
//                             material_types.extend(std::iter::repeat(material_type).take(4));
//                         }
//                     }

//                     let mut render_mesh = Mesh::new(
//                         PrimitiveTopology::TriangleList,
//                         RenderAssetUsages::RENDER_WORLD,
//                     );
//                     render_mesh.insert_attribute(
//                         Mesh::ATTRIBUTE_POSITION,
//                         VertexAttributeValues::Float32x3(positions),
//                     );
//                     render_mesh.insert_attribute(
//                         Mesh::ATTRIBUTE_NORMAL,
//                         VertexAttributeValues::Float32x3(normals),
//                     );
//                     render_mesh.insert_attribute(
//                         Mesh::ATTRIBUTE_UV_0,
//                         VertexAttributeValues::Float32x2(vec![[0.0; 2]; num_vertices]),
//                     );
//                     render_mesh.insert_attribute(
//                         ATTRIBUTE_TEX_INDEX,
//                         VertexAttributeValues::Uint32x3(material_types),
//                     );
//                     render_mesh
//                         .insert_attribute(Mesh::ATTRIBUTE_COLOR, vec![[1.0; 4]; num_vertices]);
//                     render_mesh.insert_indices(Indices::U32(indices.clone()));

//                     // The second value in this tuple is an optional component bundle.
//                     // If you want to generate some custom data for the chunk, like a nav mesh,
//                     // you can put it here in a regular Bevy component. This will then get added
//                     // to the spawned Chunk entity.
//                     // The type of this bundle is defined in the `ChunkUserBundle` associated type.
//                     (render_mesh, None)
//                 },
//             )
//         }))
//     }
// }

// fn get_voxel_fn() -> Box<dyn FnMut(IVec3) -> WorldVoxel + Send + Sync> {
//     // Set up some noise to use as the terrain height map
//     let mut noise = noise::HybridMulti::<noise::Perlin>::new(1234);
//     noise.octaves = 5;
//     noise.frequency = 1.1;
//     noise.lacunarity = 2.8;
//     noise.persistence = 0.4;

//     // We use this to cache the noise value for each y column so we only need
//     // to calculate it once per x/z coordinate
//     let mut cache = HashMap::<(i32, i32), f64>::new();

//     // Then we return this boxed closure that captures the noise and the cache
//     // This will get sent off to a separate thread for meshing by bevy_voxel_world
//     Box::new(move |pos: IVec3| {
//         // Sea level
//         if pos.y < 1 {
//             return WorldVoxel::Solid(3);
//         }

//         let [x, y, z] = pos.as_dvec3().to_array();

//         // If y is less than the noise sample, we will set the voxel to solid
//         let is_ground = y < match cache.get(&(pos.x, pos.z)) {
//             Some(sample) => *sample,
//             None => {
//                 let sample = noise.get([x / 4000.0, z / 4000.0]) * 20.0;
//                 cache.insert((pos.x, pos.z), sample);
//                 sample
//             }
//         };

//         if is_ground {
//             // Solid voxel of material type 0
//             WorldVoxel::Solid(0)
//         } else {
//             WorldVoxel::Air
//         }
//     })
//
// }
