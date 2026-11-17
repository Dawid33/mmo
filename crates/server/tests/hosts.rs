//! R3 Task 9: the real link between region hosts. Each host binds a service-link
//! listener, writes its address and pin into its `hosts` row, and dials every LOWER
//! ordinal it finds there ([`PeerLinks`]); the managers' peer sessions run over the
//! resulting [`server::peer_link::QuicPeerLink`]s. Two "processes" run in this test
//! process — each its own store handle, claims and `WorldManager<InlineSpawner>` — over
//! real links on localhost, against Postgres (`MMO_TEST_DATABASE_URL`; skipped with a
//! message when unset). Its own test binary: process-global block props, real threads
//! and sockets.

include!("common/hosts_rig.rs");

// ---- tests ------------------------------------------------------------------------------

/// Each host finds the other through the `hosts` table (address and pin of the lower
/// ordinal, dialed by the higher), and over the link they learn which regions the other
/// runs (`Volatile::Running` on Up, `RegionUp`).
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn two_hosts_find_each_other_through_the_hosts_table_and_exchange_running_sets() {
    install_props();
    let Some(db) = PgDb::create("find") else { return };
    let dir = scratch("find");
    let (mut pair, (a, b), links, claims) = two_hosts(&db, &dir);

    let rows = claims[0].store().hosts().unwrap();
    assert_eq!(rows.len(), 2, "{rows:?}");
    for (row, links) in rows.iter().zip(&links) {
        assert_eq!(row.peer_addr.as_deref(), Some(links.local_addr().to_string().as_str()), "{row:?}");
        assert_eq!(row.peer_cert.as_deref(), Some(links.cert_sha256()), "{row:?}");
        assert_eq!(row.quic, endpoint(row.id).quic);
        assert_eq!(row.webtransport, endpoint(row.id).webtransport);
    }

    let (ha, hb) = (pair.sims[0].manager.owner_host(a).expect("an owned region").0 as usize, pair.sims[0].manager.owner_host(b).expect("an owned region").0 as usize);
    pair.until("each host knows the other's seam region runs", |p| {
        p.sims[ha].manager.foreign_running().contains(&b) && p.sims[hb].manager.foreign_running().contains(&a)
    });
    drop(pair);
    drop(links);
    for c in claims {
        c.store().clone().shutdown();
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A slime past the seam on one real host lands on the other, exactly once, and
/// nothing is left unacknowledged.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn slime_crosses_between_real_hosts() {
    install_props();
    let Some(db) = PgDb::create("slime") else { return };
    let dir = scratch("slime");
    let (mut pair, (a, b), links, claims) = two_hosts(&db, &dir);
    let (ha, hb) = (pair.sims[0].manager.owner_host(a).expect("an owned region").0 as usize, pair.sims[0].manager.owner_host(b).expect("an owned region").0 as usize);
    // Both know the other's region runs (a transfer is sent only to a running owner's region).
    pair.until("running sets exchanged", |p| {
        p.sims[ha].manager.foreign_running().contains(&b) && p.sims[hb].manager.foreign_running().contains(&a)
    });
    let mut id = None;
    pair.sims[ha].manager.spawner_mut().with_region(a, |region| {
        let tick = region.current_tick();
        let pos = game::parry::math::Vector::new(REGION_SIZE + game::FLIP_HYSTERESIS + 2.0, above_ground_y(), REGION_SIZE / 2.0);
        region.with_data(|d| {
            let e = d.spawn_slime_safe(pos, 1, 0.0, tick);
            id = *d.ecs.id.try_get(e);
        });
    });
    let id = id.expect("the slime has an EntityId");
    pair.until("the slime owned by b on its host, nothing unacknowledged", |p| {
        p.sims[hb].holders(id) == [b]
            && p.sims[ha].holders(id).is_empty()
            && p.sims.iter().all(|s| s.manager.peer_unreceived() == 0)
    });
    for _ in 0..20 {
        pair.round();
    }
    assert_eq!(pair.sims[hb].holders(id), [b], "still there, once");
    assert!(pair.sims[ha].holders(id).is_empty(), "not on the sending host");
    assert_eq!(pair.sims[ha].manager.pending_arrival_total(), 0, "nothing waits on the sending host");
    assert_eq!(pair.sims[ha].manager.in_flight_count(), 0, "nothing in flight on the sending host");
    drop(pair);
    drop(links);
    for c in claims {
        c.store().clone().shutdown();
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// The breaker's client on host A (the keeper is client 0).
const BREAKER: ClientId = 1;

/// Region `rc`'s state on host `h` (as a client's join snapshot carries it).
fn region_state(p: &mut Pair, h: usize, rc: RegionCoords) -> game::Rollback {
    let mut state = None;
    p.sims[h].manager.spawner_mut().with_region(rc, |r| state = Some(r.data().clone()));
    state.expect("the region runs")
}

/// A block broken across a host seam: a player on host A, standing in the last block
/// column of A's seam region `a`, breaks the top block one column over — in B's region
/// `b`. The `BreakBlock` intent crosses the peer link and B grants it (the reply reaches
/// the breaker's body on A once): B's chunk holds air there, A's halo of `b` reads air
/// there, the ground beside and under it still stands, A's halo agrees with B's blocks
/// around it, nothing is left unacknowledged, and each host's region hashes, slot by slot,
/// equal to a replay of the events it processed applied to a snapshot taken before the
/// break (the determinism a client's state-hash check relies on).
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn a_block_broken_across_a_host_seam_changes_the_owner_and_the_halo() {
    install_props();
    let Some(db) = PgDb::create("seambreak") else { return };
    let dir = scratch("seambreak");
    let (mut pair, (a, b), links, claims) = two_hosts(&db, &dir);
    let (ha, hb) = (pair.sims[0].manager.owner_host(a).expect("an owned region").0 as usize, pair.sims[0].manager.owner_host(b).expect("an owned region").0 as usize);
    assert_ne!(ha, hb, "a host seam");
    pair.until("running sets exchanged", |p| {
        p.sims[ha].manager.foreign_running().contains(&b) && p.sims[hb].manager.foreign_running().contains(&a)
    });

    // The breaker's body in the middle of `a`'s last block column (`b` is +x of `a`).
    let voxels = game::BLOCK_VOXELS as f32;
    let mid = game::REGION_BLOCKS as i32 / 2;
    let (player, _) = claims[ha].store().resolve_player("hosts-test-breaker").unwrap();
    let record = PlayerRecord {
        client: game::Client::default(),
        pose: game::Pose::from_translation(game::rapier3d::math::Vector::new(
            REGION_SIZE - voxels / 2.0,
            above_ground_y(),
            (mid as f32 + 0.5) * voxels,
        )),
        region: a,
        adventure: None,
    };
    let now = pair.now();
    let ev = ServerEvent::ClientConnected { client: BREAKER, player, record: Some(record) };
    assert!(pair.sims[ha].manager.handle_server_event(ev, now));
    let mut body = None;
    pair.until("the breaker's body stands in a", |p| {
        p.sims[ha].manager.spawner_mut().with_region(a, |r| {
            let data = r.data();
            body = data.player_entites.get(&BREAKER).and_then(|&e| {
                let at = data.physics.bodies.get(*data.ecs.rigidbody.get(e))?.translation();
                Some((data.entity_id(e)?, at.x))
            });
        });
        body.is_some()
    });
    for _ in 0..10 {
        pair.round();
    }
    let (by, _) = body.unwrap();
    let mut x = 0.0;
    pair.sims[ha].manager.spawner_mut().with_region(a, |r| {
        let data = r.data();
        let e = *data.player_entites.get(&BREAKER).expect("the breaker stays in a");
        x = data.physics.bodies.get(*data.ecs.rigidbody.get(e)).unwrap().translation().x;
    });
    assert_eq!((x / voxels).floor() as i32, game::REGION_BLOCKS as i32 - 1, "the breaker stands in a's last block column: x = {x}");

    // The top block of `b`'s ground one column past the seam, the block beside it in `b`,
    // and the one under it.
    let top = GROUND_BLOCKS as i32 - 1;
    let target = game::WorldBlockPos::from_local(b, game::BlockPos::new(0, top, mid));
    let beside = game::WorldBlockPos::new(target.x + 1, target.y, target.z);
    let under = game::WorldBlockPos::new(target.x, target.y - 1, target.z);
    for p in [target, beside, under] {
        assert!(seen_from(&mut pair, hb, b, p).is_some(), "b's ground at {p:?}");
        assert!(seen_from(&mut pair, ha, a, p).is_some(), "a's halo holds b's ground at {p:?}");
    }

    // What a client joining now would get, then every event processed and every slot-start
    // hash from here on.
    let snapshots = [(ha, a, region_state(&mut pair, ha, a)), (hb, b, region_state(&mut pair, hb, b))];
    for sim in &mut pair.sims {
        sim.tap = Some(Vec::new());
        sim.manager.spawner_mut().record_slot_start_hashes();
    }

    // The break: the breaker's body sends `BreakBlock` at the target.
    let order = game::GameEvent::new(game::GameEventKind::Order(BREAKER, game::Order::DevBreakBlock(target)), 0, a);
    let now = pair.now();
    assert!(pair.sims[ha].manager.handle_server_event(ServerEvent::ClientPacket(game::ClientPacket::GameEvent(order), BREAKER), now));
    // An inbox holds one tick's replies, and a round runs one tick: read it every round.
    let mut replies = Vec::new();
    let read_replies = |p: &mut Pair, replies: &mut Vec<game::IntentReply>| {
        p.sims[ha].manager.spawner_mut().with_region(a, |r| replies.extend(r.data().intent_inbox(by)));
    };
    let deadline = Instant::now() + WAIT;
    while replies.is_empty()
        || seen_from(&mut pair, hb, b, target).is_some()
        || seen_from(&mut pair, ha, a, target).is_some()
        || pair.sims.iter().any(|s| s.manager.peer_unreceived() > 0)
    {
        assert!(Instant::now() < deadline, "the break never settled: replies {replies:?}");
        pair.round();
        read_replies(&mut pair, &mut replies);
    }
    for _ in 0..10 {
        pair.round();
        read_replies(&mut pair, &mut replies);
    }
    let taps: Vec<_> = pair.sims.iter_mut().map(|s| s.tap.take().unwrap()).collect();
    let slot_hashes: Vec<_> = pair.sims.iter_mut().map(|s| s.manager.spawner_mut().take_slot_start_hashes()).collect();

    assert_eq!(replies, vec![game::IntentReply::Granted(game::Intent::BreakBlock { by, survival: false })], "B granted it, once");
    assert_eq!(seen_from(&mut pair, hb, b, target), None, "b's chunk holds air there");
    assert_eq!(seen_from(&mut pair, ha, a, target), None, "a's halo reads air there");
    for p in [beside, under] {
        assert!(seen_from(&mut pair, hb, b, p).is_some(), "the ground at {p:?} still stands");
    }
    // A's halo of `b` agrees with `b`'s own blocks around the break.
    for dx in 0..game::HALO_BLOCKS {
        for y in 0..=GROUND_BLOCKS as i32 {
            for dz in -2..=2 {
                let p = game::WorldBlockPos::new(target.x + i64::from(dx), y, target.z + i64::from(dz));
                assert_eq!(seen_from(&mut pair, ha, a, p), seen_from(&mut pair, hb, b, p), "a's halo and b at {p:?}");
            }
        }
    }
    assert_eq!(pair.sims[ha].manager.in_flight_count(), 0, "nothing in flight on host A");
    assert_eq!(pair.sims[hb].manager.in_flight_count(), 0, "nothing in flight on host B");

    // Each host's region against a replay of what it processed since the snapshot.
    for (h, rc, snapshot) in snapshots {
        let (send, _updates) = unbounded();
        let mut mirror = game::Region::new(snapshot, Some(send), rc, None);
        let events: Vec<_> = taps[h].iter().filter(|(r, _)| *r == rc).map(|(_, ev)| ev.clone()).collect();
        assert!(!events.is_empty(), "{rc:?} processed events");
        // Every slot-start hash the live region recorded, against the replay's after the
        // same Tick.
        let recorded: Vec<(game::Tick, u32)> = slot_hashes[h].iter().filter(|(r, ..)| *r == rc).map(|(_, t, x)| (*t, *x)).collect();
        let mut compared = 0;
        for ev in events {
            let (id, is_tick) = (ev.id, matches!(ev.kind, game::GameEventKind::Tick));
            assert_eq!(mirror.handle_event(ev.kind).unwrap().id, id, "the replay numbers events alike");
            mirror.commit_server_event();
            if is_tick {
                let t = mirror.current_tick();
                if let Some((_, live)) = recorded.iter().find(|(tick, _)| *tick == t) {
                    assert_eq!(game::state_hash(mirror.data()), *live, "host {h}'s {rc:?} at tick {t} hashes equal to its replay");
                    compared += 1;
                }
            }
        }
        assert_eq!(compared, recorded.len(), "every recorded slot of {rc:?} was replayed");
        let mut live = 0;
        pair.sims[h].manager.spawner_mut().with_region(rc, |r| live = game::state_hash(r.data()));
        assert_eq!(game::state_hash(mirror.data()), live, "host {h}'s {rc:?} hashes equal to its replay");
        let replayed = mirror.data().halo_block(target).map(|b| b.id);
        assert_eq!(replayed, Some(game::BlockId::AIR), "the replay of {rc:?} reads air at the break too");
    }

    drop(pair);
    drop(links);
    for c in claims {
        c.store().clone().shutdown();
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Elastic 1: two host processes started with the SAME environment — no ordinal, no host
/// count, no name (`VOXEL_HOST_NAME` empty), the same advertised addresses; only each one's
/// peer bind and pin file are its own — get two ids from the store, both rows live (an empty
/// name lapses nobody), and their peer link comes up (the newer dials the older, which admits
/// it by its live row). Ready (`/readyz`) follows the registration in `server::run`; a
/// process-wide health state cannot be shown twice in one test process.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn two_hosts_started_with_the_same_environment_get_two_ids() {
    let Some(db) = PgDb::create("sameenv") else { return };
    let dir = scratch("sameenv");
    let register = |boot: u64, dir: &std::path::Path| {
        let store = db.open();
        let config = PeerLinkConfig { bind: "127.0.0.1:0".parse().unwrap(), cert_hash_file: dir.join(format!("{boot}.pin")), delay: Duration::ZERO, world: String::new() };
        let listener = PeerListener::bind(config, TOKEN.into()).unwrap();
        let (peer_addr, peer_cert) = listener.row_fields();
        let host = world_store::NewHost {
            name: String::new(),
            boot,
            peer_addr: Some(peer_addr),
            peer_cert: Some(peer_cert),
            quic: Some("127.0.0.1:6466".into()),
            webtransport: None,
            wt_cert_hash: None,
        };
        let stop = std::sync::atomic::AtomicBool::new(false);
        let registered = server::lease::register(&store, host, CrashPolicy::Succession, &stop).unwrap().expect("registered");
        let links = PeerLinks::start(listener, registered.id, boot, store.clone()).unwrap();
        (store, links, registered)
    };
    let (s1, mut l1, r1) = register(11, &dir);
    let (s2, mut l2, r2) = register(12, &dir);
    assert_ne!(r1.id, r2.id, "two ids");
    assert!(!r1.bumped && !r2.bumped && r1.generation == r2.generation);
    let rows = s1.hosts().unwrap();
    for id in [r1.id, r2.id] {
        let row = rows.iter().find(|r| r.id == id).expect("a row");
        assert!(row.state == game::HOST_LIVE && !row.clean && row.seen_ms != 0, "{row:?} live, not lapsed");
    }
    let (o2, mut to1) = only_link(&mut l2);
    let (o1, mut to2) = only_link(&mut l1);
    assert_eq!((o1, o2), (r2.id, r1.id));
    pump_until("the link up", WAIT, &mut [&mut to1, &mut to2], |l| l.iter().all(|l| l.is_up()));
    drop((to1, to2, l1, l2));
    s1.shutdown();
    s2.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A host dialing with another service token never comes up on either side.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn peer_with_a_wrong_token_is_refused() {
    let Some(db) = PgDb::create("token") else { return };
    let dir = scratch("token");
    let (s0, l0, _c0) = fixed_host(&db, &dir, 1, TOKEN, "127.0.0.1:0");
    let (s1, mut l1, _c1) = fixed_host(&db, &dir, 2, "not-the-token", "127.0.0.1:0");
    // The newer host dials the older one; the older makes a link only for a host it admits.
    let (o1, mut to0) = only_link(&mut l1);
    assert_eq!(o1, h(0));
    // Long enough for the dialer to find the row and try a few times.
    let deadline = Instant::now() + Duration::from_secs(4);
    while Instant::now() < deadline {
        to0.pump(0);
        assert!(!to0.is_up(), "a peer with a wrong token came up");
        assert!(l0.take_arrived().is_empty(), "the older host admitted a peer with a wrong token");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(to0.connection(), 0);
    drop((to0, l0, l1));
    s0.shutdown();
    s1.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

/// Elastic 1 Task 4: a dial with the right service token and a hello naming a host id that has
/// no live `hosts` row (never issued, or another boot of a live id) is refused: the listener
/// closes it and no link is made for it; a hello of a live row is admitted.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn an_unregistered_peer_is_refused() {
    let Some(db) = PgDb::create("unreg") else { return };
    let dir = scratch("unreg");
    let (s0, l0, _c0) = fixed_host(&db, &dir, 1, TOKEN, "127.0.0.1:0");
    let (addr, pin) = (l0.local_addr(), l0.cert_sha256().to_string());
    let dial = |id: u64, boot: u64| {
        let hello = svc_link::LinkHello::new(TOKEN.into(), svc_link::PeerRole::Host { id: h(id), boot, reg: id }, "");
        svc_link::LinkDialer::connect::<PeerMsg, PeerMsg>(addr, pin.clone(), hello)
    };
    // Id 41 was never issued; id 0 is host 0 itself (dialed by nobody).
    for (id, boot) in [(41, 77), (0, 1)] {
        let (_out, events) = dial(id, boot);
        let deadline = Instant::now() + Duration::from_secs(4);
        let mut closed = false;
        while Instant::now() < deadline && !closed {
            if let Ok(svc_link::LinkEvent::Down(_)) = events.recv_timeout(Duration::from_millis(50)) {
                closed = true;
            }
            assert!(l0.take_arrived().is_empty(), "host {id} (no live row) was admitted");
        }
        assert!(closed, "the listener closed the dial of host {id}");
    }
    // A host registered after host 0 started (a live row, found by the admission's one read
    // on a miss) is admitted: its link arrives.
    let (s1, l1, _c1) = fixed_host(&db, &dir, 2, TOKEN, "127.0.0.1:0");
    let deadline = Instant::now() + WAIT;
    let mut arrived = Vec::new();
    while Instant::now() < deadline && arrived.is_empty() {
        arrived = l0.take_arrived();
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(arrived.iter().map(|(id, _)| *id).collect::<Vec<_>>(), [h(1)], "the registered host is admitted");
    drop((arrived, l0, l1));
    s0.shutdown();
    s1.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

/// The lower host restarts on another port (a new boot): Elastic 1 — the restart is a NEW host
/// with a higher id, so it dials the host that was higher before (admitted by its live row)
/// and is connected within 5 s, and frames flow; the old id is never dialed again.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn restarted_listener_is_found_again() {
    let Some(db) = PgDb::create("restart") else { return };
    let dir = scratch("restart");
    let (s0, mut l0, _c0) = fixed_host(&db, &dir, 1, TOKEN, "127.0.0.1:0");
    let (s1, mut l1, _c1) = fixed_host(&db, &dir, 2, TOKEN, "127.0.0.1:0");
    let old_addr = l0.local_addr();
    let (_, mut to1) = only_link(&mut l0);
    let (_, mut to0) = only_link(&mut l1);
    pump_until("both ends up", WAIT, &mut [&mut to1, &mut to0], |l| l.iter().all(|l| l.is_up()));
    assert_eq!(to0.connection(), 1);
    to0.send(PeerMsg::Volatile(Volatile::Running(vec![RegionCoords::new(1, 2)])));
    pump_until("a frame from 1 to 0", WAIT, &mut [&mut to1], |l| {
        l[0].recv() == Some(PeerMsg::Volatile(Volatile::Running(vec![RegionCoords::new(1, 2)])))
    });

    // Host 0 stops: its listener goes, its port is freed.
    drop(to1);
    drop(l0);
    s0.shutdown();
    pump_until("host 1 sees the link down", WAIT, &mut [&mut to0], |l| !l[0].is_up());

    // Host 0 starts again: a new claim, a new boot, another port.
    let new_bind = loop {
        let addr = std::net::UdpSocket::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
        if addr != old_addr {
            break addr;
        }
    };
    // Host 1 runs on (its lease beaten): the restart is not a total crash.
    assert!(s1.heartbeat(h(1), 2).unwrap().live, "host 1's row is its live boot");
    let store2 = db.open();
    let (mut l2, again) = boot_links(&store2, next_id(&db), "host-0", 3, TOKEN, &dir, &new_bind.to_string());
    assert_eq!((again.id, l2.local_addr()), (h(2), new_bind), "a new id: the next one");
    // It dials every older live row: host 1, and host 0's lapsed one (dead once declared).
    let mut dialed = l2.take();
    assert_eq!(dialed.iter().map(|(o, _)| *o).collect::<Vec<_>>(), vec![h(0), h(1)], "the older live rows");
    let (to, mut to1) = dialed.pop().unwrap();
    assert_eq!(to, h(1), "the new host dials the older one");
    let (from, mut to2) = only_link(&mut l1);
    assert_eq!(from, h(2), "the older one admits it by its live row");
    pump_until("connected within 5 s", Duration::from_secs(5), &mut [&mut to1, &mut to2], |l| l.iter().all(|l| l.is_up()));
    to2.send(PeerMsg::Ack { through: 7 });
    pump_until("a frame over the new link", WAIT, &mut [&mut to1], |l| l[0].recv() == Some(PeerMsg::Ack { through: 7 }));
    assert_eq!(query_i64s(&db, "SELECT seen_ms FROM hosts WHERE id = 0"), [0], "its old row lapsed by its name");
    drop((to1, to0, to2, l2, l1));
    store2.shutdown();
    s1.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

// ---- world cuts (Task 13) ----------------------------------------------------------------


/// Two real hosts and a real coordinator: a world cut is picked by the coordinator,
/// staged by both hosts and committed through one decision — both hosts' seam regions
/// carry its tick, one committed row is in `cut_decisions`, and nothing stays prepared.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn two_hosts_commit_one_world_cut() {
    install_props();
    let Some(db) = PgDb::create("wcut") else { return };
    let dir = scratch("wcut");
    let w = world(&db, &dir);
    let (a, b) = w.seam;
    let World { mut pair, coordinator, links, claims, store, .. } = w;
    let coordinator = coordinator.unwrap();
    pair.until("a world cut committed", |_| decisions(&db).iter().any(|(_, _, c)| *c));
    pair.until("both hosts settled it", |p| p.sims.iter().all(|s| s.cuts.as_ref().unwrap().world_part().is_none()));
    let committed: Vec<(i64, i64, bool)> = decisions(&db).into_iter().filter(|(_, _, c)| *c).collect();
    let (cut, tick, _) = committed[0];
    for rc in [a, b] {
        let saved = query_i64s(&db, &format!("SELECT saved_tick FROM regions WHERE rx = {} AND rz = {}", rc.x, rc.z));
        assert!(!saved.is_empty(), "{rc:?} is stored");
        assert!(saved[0] >= tick, "{rc:?} carries the cut's tick ({} < {tick})", saved[0]);
    }
    let first: Vec<i64> = committed.iter().filter(|(c, _, _)| *c == cut).map(|(_, t, _)| *t).collect();
    assert_eq!(first.len(), 1, "one decision row for cut {cut}");
    assert_eq!(prepared(&db), 0, "nothing stays prepared");
    // Each committed cut's tick is on both seam regions as of that cut (the latest one).
    let latest = committed.last().unwrap().1;
    for rc in [a, b] {
        let saved = query_i64s(&db, &format!("SELECT saved_tick FROM regions WHERE rx = {} AND rz = {}", rc.x, rc.z))[0];
        let any_later = decisions(&db).iter().any(|(_, t, c)| *c && *t > latest);
        assert!(saved == latest || any_later, "{rc:?} saved at {saved}, the latest committed cut is at {latest}");
    }
    assert_eq!(coordinator.thread.shutdown(Duration::from_secs(10)), server::claims::ServerExit::Clean);
    drop(pair);
    drop(links);
    store.shutdown();
    for c in claims {
        c.store().clone().shutdown();
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Host 1's link to the coordinator drops while a world cut runs: the cut is aborted
/// (a decision row with `committed = 0`), host 0's part — staged or not — is rolled back
/// (nothing stays prepared) and settled on host 0; once host 1 is back the next cut
/// commits.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn a_cut_with_one_host_detached_is_aborted_and_leaves_nothing_staged() {
    install_props();
    let Some(db) = PgDb::create("wdet") else { return };
    let dir = scratch("wdet");
    let World { mut pair, coordinator, cert, links, claims, store, switches, .. } = world(&db, &dir);
    let coordinator = coordinator.unwrap();
    pair.until("host 0 takes part in a world cut", |p| p.sims[0].manager.world_cut().is_some());
    let running = coordinator.thread.inspect(|c| c.world_cut_running(), WAIT).flatten().expect("a world cut runs");
    switches[1].drop_link();
    pair.until("the cut is decided", |_| decisions(&db).iter().any(|(c, _, _)| *c as u64 == running.0 .0));
    let row = decisions(&db).into_iter().find(|(c, _, _)| *c as u64 == running.0 .0).unwrap();
    assert!(!row.2, "the cut with a host detached is aborted");
    pair.until("host 0 settled it", |p| {
        p.sims[0].cuts.as_ref().unwrap().world_part().is_none() && p.sims[0].manager.world_cut() != Some(running.0)
    });
    assert_eq!(prepared(&db), 0, "nothing stays prepared");
    // Host 1 back: the next cut commits.
    switches[1].replace(quic_world_link(coordinator.addr, cert, h(1), 1001, pair.sims[1].reg));
    pair.until("a later world cut committed", |_| decisions(&db).iter().any(|(c, _, k)| *k && *c as u64 > running.0 .0));
    pair.until("both hosts settled it", |p| p.sims.iter().all(|s| s.cuts.as_ref().unwrap().world_part().is_none()));
    assert_eq!(prepared(&db), 0, "nothing stays prepared");
    assert_eq!(coordinator.thread.shutdown(Duration::from_secs(10)), server::claims::ServerExit::Clean);
    drop(pair);
    drop(links);
    store.shutdown();
    for c in claims {
        c.store().clone().shutdown();
    }
    let _ = std::fs::remove_dir_all(&dir);
}


/// R4 Task 12 (real links): the peer link between the two hosts is closed (`PeerGate`)
/// and stays closed while a world cut that began after it commits — each host's count of
/// the other relayed through the real coordinator — and nothing stays prepared. A slime
/// crosses the seam toward the other host while the link is down: it waits in the sender's
/// frozen session, above the receiver's count, so that cut stores it once — as the
/// sender's in-flight row. The link back, it lands once, the next cut commits too, and it is
/// stored once (in its region now).
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn a_cut_with_a_peer_link_down_commits() {
    install_props();
    let Some(db) = PgDb::create("wcount") else { return };
    let dir = scratch("wcount");
    let mut w = world(&db, &dir);
    let (a, b) = w.seam;
    let (ha, hb) = (w.pair.sims[0].manager.owner_host(a).expect("owned").0 as usize, w.pair.sims[0].manager.owner_host(b).expect("owned").0 as usize);
    w.until_committed_after(&db, 0);
    for sim in &w.pair.sims {
        sim.gate.set_down(true);
    }
    w.pair.until("the peer link is seen down", |p| !p.sims[0].manager.peer_is_reachable(h(1)) && !p.sims[1].manager.peer_is_reachable(h(0)));
    let id = spawn_slime_in(&mut w, HostId(ha as u64), a, REGION_SIZE + game::FLIP_HYSTERESIS + 2.0);
    w.pair.until("the slime waits for the other host", |p| p.sims[ha].manager.peer_unreceived_ids().iter().any(|(_, i, _)| *i == id));
    let coordinator = w.coordinator.as_ref().unwrap();
    let running = coordinator.thread.inspect(|c| c.world_cut_running(), WAIT).flatten().map_or(0, |(c, _)| c.0 as i64);
    let decided = decisions(&db).iter().map(|(c, _, _)| *c).max().unwrap_or(0);
    let after = running.max(decided);
    w.until_committed_after(&db, after);
    assert!(w.pair.sims.iter().all(|s| s.gate.is_down()), "the link stayed down");
    assert!(!w.pair.sims[0].manager.peer_is_reachable(h(1)), "host 0 never reached host 1 meanwhile");
    assert_eq!(prepared(&db), 0, "nothing stays prepared");
    assert_eq!(stored_times(&w, &db, id), (0, 1), "the slime is the sender's in-flight row, once");
    for sim in &w.pair.sims {
        sim.gate.set_down(false);
    }
    w.pair.until("the slime landed on the other host, once", |p| {
        p.sims[hb].holders(id) == [b] && p.sims[ha].holders(id).is_empty() && p.sims.iter().all(|s| s.manager.peer_unreceived() == 0)
    });
    let latest = decisions(&db).iter().map(|(c, _, _)| *c).max().unwrap_or(0);
    w.until_committed_after(&db, latest);
    assert_eq!(prepared(&db), 0, "nothing stays prepared");
    assert_eq!(stored_times(&w, &db, id), (1, 0), "the slime is stored once, in its region");
    w.stop(&dir);
}

/// How often the store holds `id`: `(region rows, in-flight rows)`.
fn stored_times(w: &World, db: &PgDb, id: EntityId) -> (usize, usize) {
    let rows = query_i64s(
        db,
        &format!(
            "SELECT COUNT(*) FROM entities WHERE birth_rx = {} AND birth_rz = {} AND serial = {}",
            id.birth_region.x, id.birth_region.z, id.serial
        ),
    )[0] as usize;
    let flying = w
        .store
        .load_in_flight(None)
        .unwrap()
        .iter()
        .filter(|r| matches!(r, game::InFlightRow::Transfer { bundle, .. } if bundle.id == id))
        .count();
    (rows, flying)
}

/// Important 2: the coordinator's decision write and its first abort both fail: it keeps
/// trying until an abort is stored, announces only that, and the next cut commits on
/// both hosts — nothing stays staged.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn a_failed_decision_does_not_stall_later_cuts() {
    install_props();
    let Some(db) = PgDb::create("wfail") else { return };
    let dir = scratch("wfail");
    let mut w = world(&db, &dir);
    w.store.fail_next_decisions(2);
    w.pair.until("a world cut decided", |_| !decisions(&db).is_empty());
    let first = decisions(&db)[0];
    assert!(!first.2, "the cut whose decision failed is stored aborted");
    assert_eq!(w.store.fail_next_decisions_left(), 0, "the decision and one abort failed first");
    w.until_committed_after(&db, first.0);
    assert_eq!(prepared(&db), 0, "nothing stays prepared");
    w.stop(&dir);
}

/// Important 2: a host told its cut is aborted while the store has no decision (the
/// coordinator cannot write one) records the abort itself and settles; the coordinator's
/// later decision finds it; the next cut commits once the store works again.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn host_told_aborted_without_a_stored_decision_aborts_it_itself() {
    install_props();
    let Some(db) = PgDb::create("wself") else { return };
    let dir = scratch("wself");
    let mut w = world(&db, &dir);
    w.store.fail_next_decisions(u32::MAX);
    let cut = until_part(&mut w, 0, &["prepared"]);
    assert_eq!(decision_of(&db, cut), None, "undecided");
    w.coordinator.as_ref().unwrap().thread.with_core(move |c| c.cut_outcome(h(0), cut, false), WAIT).expect("the core answers");
    w.pair.until("host 0 settled its part", |p| p.sims[0].cuts.as_ref().unwrap().world_part().is_none());
    assert_eq!(decision_of(&db, cut), Some(false), "host 0 recorded the abort");
    w.pair.until("host 1 settled its part too", |p| p.sims[1].cuts.as_ref().unwrap().world_part().is_none());
    w.store.fail_next_decisions(0);
    w.until_committed_after(&db, cut.0 as i64);
    assert_eq!(prepared(&db), 0, "nothing stays prepared");
    w.stop(&dir);
}

/// Criterion D: a host's handle still holds an undecided part of another cut (`CutPending`):
/// its world part fails, and once that other cut is decided the host settles it and the
/// next cut commits.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn cut_pending_on_a_host_settles_that_cut_first() {
    install_props();
    let Some(db) = PgDb::create("wpend") else { return };
    let dir = scratch("wpend");
    let mut w = world(&db, &dir);
    let other = game::CutId(77);
    let generation = w.generations[0].mine();
    let empty = game::WorldCut { id: other, host: h(0), tick: 1, regions: vec![], in_flight: vec![], players: vec![], history: None, adopts: vec![] };
    let staged = w.claims[0].store().prepare_cut(empty, (*w.claims[0].fence()).clone(), generation).recv().unwrap();
    assert!(staged.is_ok(), "the other cut's part is staged: {staged:?}");
    w.pair.until("a world cut decided", |_| decisions(&db).iter().any(|(c, _, _)| *c as u64 != other.0));
    let failed = decisions(&db).into_iter().find(|(c, _, _)| *c as u64 != other.0).unwrap();
    assert!(!failed.2, "host 0 could not stage its part: the cut is aborted");
    assert_eq!(w.pair.sims[0].cuts.as_ref().unwrap().world_part(), Some(other), "host 0 now settles cut {}", other.0);
    w.store.abort_cut(other, 1).unwrap();
    w.until_committed_after(&db, failed.0);
    assert_eq!(prepared(&db), 0, "nothing stays prepared");
    w.stop(&dir);
}

/// Criterion D: the world moved to a newer generation while a cut was pending: the host's
/// prepare is refused `StaleGeneration`, the host notes the newer generation (it exits
/// for the rollback), and the cut is aborted.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn stale_generation_on_prepare_is_noted() {
    install_props();
    let Some(db) = PgDb::create("wstale") else { return };
    let dir = scratch("wstale");
    let mut w = world(&db, &dir);
    let cut = until_part(&mut w, 0, &["pending"]);
    exec(&db, "UPDATE world SET generation = generation + 1");
    w.pair.until("the cut decided", |_| decision_of(&db, cut).is_some());
    assert_eq!(decision_of(&db, cut), Some(false), "aborted");
    w.pair.until("host 0 noted the newer generation", |_| w.generations[0].is_set());
    assert_eq!(prepared(&db), 0, "nothing stays prepared");
    w.stop(&dir);
}

/// Criterion D: host 1's prepare waits on a lock another transaction holds past its lock
/// timeout: the part fails (nothing staged), the cut is aborted, and once the lock is
/// gone the next cut commits.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn lock_timeout_on_prepare_fails_the_part_and_the_next_cut_commits() {
    install_props();
    let Some(db) = PgDb::create("wlock") else { return };
    let dir = scratch("wlock");
    let mut w = world(&db, &dir);
    w.claims[1].store().set_cut_lock_timeout_ms(200);
    // A transaction holding host 1's super-region rows until released.
    let (locked_tx, locked) = crossbeam::channel::bounded::<()>(1);
    let (release_tx, release) = crossbeam::channel::bounded::<()>(1);
    let url = db.0.url.clone();
    let holder = std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            use sqlx::Connection;
            let mut c = sqlx::PgConnection::connect(&url).await.unwrap();
            let mut tx = c.begin().await.unwrap();
            sqlx::query("SELECT * FROM super_regions WHERE owner = 1 FOR UPDATE").execute(&mut *tx).await.unwrap();
            locked_tx.send(()).unwrap();
            let _ = release.recv();
            tx.rollback().await.unwrap();
        })
    });
    locked.recv().unwrap();
    w.pair.until("a world cut decided", |_| !decisions(&db).is_empty());
    let first = decisions(&db)[0];
    assert!(!first.2, "host 1's part timed out on the lock: aborted");
    release_tx.send(()).unwrap();
    holder.join().unwrap();
    w.claims[1].store().set_cut_lock_timeout_ms(world_store::CUT_LOCK_TIMEOUT_MS);
    w.until_committed_after(&db, first.0);
    assert_eq!(prepared(&db), 0, "nothing stays prepared");
    w.stop(&dir);
}

/// Criterion D: host 1's `CutDone` is lost (its coordinator link dropped right after it
/// staged its part): its poll finds the stored decision and settles while the link is
/// still down.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn lost_cut_done_is_settled_from_the_store() {
    install_props();
    let Some(db) = PgDb::create("wlost") else { return };
    let dir = scratch("wlost");
    let mut w = world(&db, &dir);
    let cut = until_part(&mut w, 1, &["prepared"]);
    w.switches[1].drop_link();
    w.pair.until("the cut decided", |_| decision_of(&db, cut).is_some());
    w.pair.until("host 1 settled its part with its link down", |p| p.sims[1].cuts.as_ref().unwrap().world_part().is_none());
    assert!(!w.pair.sims[1].manager.world_attached(), "host 1's link is still down");
    assert_eq!(prepared(&db), 0, "nothing stays prepared");
    let link = quic_world_link(w.coordinator.as_ref().unwrap().addr, w.cert.clone(), h(1), 1001, w.pair.sims[1].reg);
    w.switches[1].replace(link);
    w.until_committed_after(&db, cut.0 as i64);
    w.stop(&dir);
}

/// Fix round 2: a host that hears nothing about its staged part past the deadline (the
/// coordinator cannot store a decision, so it announces none) records the abort itself
/// and settles; once decisions work again the next cut commits.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn host_hearing_nothing_past_the_deadline_aborts_the_cut_itself() {
    install_props();
    let Some(db) = PgDb::create("wdead") else { return };
    let dir = scratch("wdead");
    let mut w = world(&db, &dir);
    w.store.fail_next_decisions(u32::MAX);
    w.pair.sims[0].cuts.as_mut().unwrap().set_world_part_deadline(Duration::from_secs(2));
    let cut = until_part(&mut w, 0, &["prepared"]);
    assert_eq!(decision_of(&db, cut), None, "undecided");
    w.pair.until("host 0 settled its part on its own", |p| p.sims[0].cuts.as_ref().unwrap().world_part().is_none());
    assert_eq!(decision_of(&db, cut), Some(false), "host 0 recorded the abort past its deadline");
    w.store.fail_next_decisions(0);
    w.until_committed_after(&db, cut.0 as i64);
    assert_eq!(prepared(&db), 0, "nothing stays prepared");
    w.stop(&dir);
}

// ---- leaving and crashes (Task 14) -----------------------------------------------------


/// Crash table: host 1 dies after its part is staged and the coordinator holds every
/// `CutPrepared` (before the decision): the cut commits whole; the static world restarts as
/// one new host (a total crash: the generation + 1) from that cut.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn host_killed_after_prepare_cut_commits_and_world_restarts_from_it() {
    install_props();
    let Some(db) = PgDb::create("kprep") else { return };
    let dir = scratch("kprep");
    let mut w = world(&db, &dir);
    let generation = w.claims[0].store().world_generation().unwrap();
    // Host 1's part staged, and the coordinator holds every `CutPrepared` of it (or has
    // already recorded the commit: the race with the decision's write is the test's).
    let mut cut = None;
    let deadline = Instant::now() + WAIT;
    while cut.is_none() {
        assert!(Instant::now() < deadline, "no cut had every part staged");
        if let Some((c, "prepared")) = sim(&mut w, h(1)).manager.world_cut_state() {
            let progress = w.coordinator.as_ref().unwrap().thread.inspect(|c| c.world_cut_progress(), WAIT).flatten();
            let all_at_core = progress.is_some_and(|(r, prepared, _)| r == c && prepared == [h(0), h(1)]);
            if all_at_core || decision_of(&db, c) == Some(true) {
                cut = Some(c);
                break;
            }
        }
        w.pair.round();
    }
    let cut = cut.unwrap();
    kill_host(&mut w, h(1));
    w.pair.until("the cut decided", |_| decision_of(&db, cut).is_some());
    assert_eq!(decision_of(&db, cut), Some(true), "every part was staged: committed");
    let (_, tick) = last_committed(&db);
    w.pair.until("host 0 settled it", |p| p.sims[0].cuts.as_ref().unwrap().world_part().is_none());
    assert_eq!(prepared(&db), 0, "nothing stays prepared");
    // Elastic 1: a static world is one host — it recovers as a total crash: the whole world
    // restarts as ONE new host (a new id), the generation raised exactly once.
    let again = restart_alone(&mut w, &db, &dir, 3001);
    assert_eq!((again.generation, again.bumped), (generation + 1, true), "a total crash: + 1");
    assert_eq!(w.claims[0].store().world_generation().unwrap(), generation + 1, "raised exactly once");
    let (a, b) = w.seam;
    let (last, _) = last_committed(&db);
    if last as u64 == cut.0 {
        for rc in [a, b] {
            assert_eq!(saved_tick(&db, rc), tick, "{rc:?} is as of the cut");
        }
    }
    w.until_committed_after(&db, last);
    assert_eq!(prepared(&db), 0, "nothing stays prepared");
    w.stop(&dir);
}

/// Crash table: host 1 dies before its part is staged (host 0's may be): the cut is
/// aborted (host 1's detach), nothing stays prepared; the static world restarts as one new
/// host (a total crash) from the previous cut.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn host_killed_before_prepare_cut_is_absent_and_the_world_rolls_back() {
    install_props();
    let Some(db) = PgDb::create("kpend") else { return };
    let dir = scratch("kpend");
    let mut w = world(&db, &dir);
    let generation = w.claims[0].store().world_generation().unwrap();
    w.until_committed_after(&db, 0);
    let cut = until_part(&mut w, 1, &["pending", "marked"]);
    kill_host(&mut w, h(1));
    w.pair.until("the cut decided", |_| decision_of(&db, cut).is_some());
    assert_eq!(decision_of(&db, cut), Some(false), "a part never staged: aborted");
    w.pair.until("host 0 settled it", |p| p.sims[0].cuts.as_ref().unwrap().world_part().is_none());
    assert_eq!(prepared(&db), 0, "nothing stays prepared");
    let (last, tick) = last_committed(&db);
    // Elastic 1: a static world is one host — it recovers as a total crash: the whole world
    // restarts as ONE new host (a new id), the generation raised exactly once.
    let again = restart_alone(&mut w, &db, &dir, 3001);
    assert_eq!((again.generation, again.bumped), (generation + 1, true), "a total crash: + 1");
    assert_eq!(w.claims[0].store().world_generation().unwrap(), generation + 1, "raised exactly once");
    if last_committed(&db).0 == last {
        let (a, b) = w.seam;
        for rc in [a, b] {
            assert_eq!(saved_tick(&db, rc), tick, "{rc:?} is as of the previous cut");
        }
    }
    w.until_committed_after(&db, last);
    assert_eq!(prepared(&db), 0, "nothing stays prepared");
    w.stop(&dir);
}

/// Crash table: the coordinator dies with every part staged and no decision stored (its
/// decision writes fail): its restart resolves the staged parts first — an abort is
/// stored, nothing stays prepared — and the hosts, re-attached, hear `CutDone(false)`,
/// keep everything uncommitted and commit the next cut.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn coordinator_killed_before_the_decision_cut_is_absent() {
    install_props();
    let Some(db) = PgDb::create("kcdec") else { return };
    let dir = scratch("kcdec");
    let mut w = world(&db, &dir);
    w.store.fail_next_decisions(u32::MAX);
    let cut = until_part(&mut w, 0, &["prepared"]);
    w.pair.until("host 1 staged its part too", |p| matches!(p.sims[1].manager.world_cut_state(), Some((c, "prepared")) if c == cut));
    assert_eq!(decision_of(&db, cut), None, "undecided");
    assert_eq!(prepared(&db), 2, "both parts staged");
    kill_coordinator(&mut w);
    let store = db.open();
    server::roles::recover_staged_cuts(&store).expect("the staged parts are resolved");
    assert_eq!(decision_of(&db, cut), Some(false), "the restart aborted the undecided cut");
    assert_eq!(prepared(&db), 0, "nothing stays prepared");
    restart_coordinator_listening(&mut w, store);
    w.pair.until("both hosts heard the outcome and settled", |p| {
        p.sims.iter().all(|s| s.manager.world_attached() && s.manager.world_cut() != Some(cut) && s.cuts.as_ref().unwrap().world_part().is_none())
    });
    w.until_committed_after(&db, cut.0 as i64);
    assert_eq!(prepared(&db), 0, "nothing stays prepared");
    w.stop(&dir);
}

/// Crash table: the coordinator records a commit and dies after the first `COMMIT
/// PREPARED` (`stop_after_commit_prepared(1)`): one part is still prepared. Its restart
/// commits the rest before it does anything else — nothing stays prepared and both
/// hosts' seam regions carry the cut's tick — and the hosts settle it as committed.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn coordinator_killed_between_commit_prepareds_cut_is_whole() {
    install_props();
    let Some(db) = PgDb::create("kccom") else { return };
    let dir = scratch("kccom");
    let mut w = world(&db, &dir);
    w.store.stop_after_commit_prepared(1);
    // The failed decision is never retried (the coordinator is to die right after it).
    w.coordinator.as_ref().unwrap().thread.hold_decision_retries();
    // Both parts staged: no host round from here on (a host's settling would finish the
    // rest itself); the coordinator decides on its own thread.
    let c = until_part(&mut w, 0, &["prepared"]);
    w.pair.until("host 1 staged its part too", |p| matches!(p.sims[1].manager.world_cut_state(), Some((k, "prepared")) if k == c));
    let deadline = Instant::now() + WAIT;
    let (cut, tick) = loop {
        assert!(Instant::now() < deadline, "no commit was recorded");
        if let Some((c, t, _)) = decisions(&db).into_iter().find(|(_, _, k)| *k) {
            break (c, t);
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(cut as u64, c.0, "the staged cut committed");
    // The decision's write failed after one COMMIT PREPARED; nothing finishes the other.
    let deadline = Instant::now() + Duration::from_secs(2);
    while prepared(&db) != 1 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    kill_coordinator(&mut w);
    assert_eq!(prepared(&db), 1, "the coordinator died with one part still prepared");
    let store = db.open();
    server::roles::recover_staged_cuts(&store).expect("the staged parts are resolved");
    assert_eq!(prepared(&db), 0, "the restart committed the rest");
    let (a, b) = w.seam;
    for rc in [a, b] {
        assert!(saved_tick(&db, rc) >= tick, "{rc:?} carries the cut's tick");
    }
    restart_coordinator_listening(&mut w, store);
    w.pair.until("both hosts settled it", |p| {
        p.sims.iter().all(|s| s.manager.world_cut().map_or(true, |c| c.0 as i64 != cut) && s.cuts.as_ref().unwrap().world_part().is_none())
    });
    w.until_committed_after(&db, cut);
    assert_eq!(prepared(&db), 0, "nothing stays prepared");
    w.stop(&dir);
}

/// Crash table: host 1 dies between cuts (nothing staged) with a slime it took over since
/// the last cut; the static world restarts as one new host (Elastic 1: a total crash — the
/// generation raised exactly once), and the slime is in the world exactly once (where the
/// last cut has it).
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn dirty_restart_rolls_every_host_back_once() {
    install_props();
    let Some(db) = PgDb::create("kdirty") else { return };
    let dir = scratch("kdirty");
    let mut w = world(&db, &dir);
    let generation = w.claims[0].store().world_generation().unwrap();
    let a0 = seam_of(&w, h(0));
    let id = spawn_slime_in(&mut w, h(0), a0, REGION_SIZE / 2.0);
    // A cut with the slime in host 0's region.
    w.until_committed_after(&db, 0);
    let (first, _) = last_committed(&db);
    w.until_committed_after(&db, first);
    // Over the seam to host 1, then host 1 dies between cuts.
    let b1 = seam_of(&w, h(1));
    let past = if b1.x > a0.x { REGION_SIZE + game::FLIP_HYSTERESIS + 2.0 } else { -game::FLIP_HYSTERESIS - 2.0 };
    let along_x = b1.x != a0.x;
    sim(&mut w, h(0)).manager.spawner_mut().with_region(a0, |region| {
        let key = region.data().id_index[&id];
        let y = above_ground_y();
        let at = if along_x {
            game::rapier3d::math::Vector::new(past, y, REGION_SIZE / 2.0)
        } else {
            game::rapier3d::math::Vector::new(REGION_SIZE / 2.0, y, past)
        };
        region.with_data(|d| d.set_body_pose_safe(key, game::Pose::from_translation(at)));
    });
    w.pair.until("the slime on host 1, no cut pending", |p| {
        let on_1 = p.sims[1].holders(id) == [b1];
        on_1 && p.sims.iter().all(|s| s.manager.world_cut().is_none() && s.manager.peer_unreceived() == 0)
    });
    kill_host(&mut w, h(1));
    // Elastic 1: a static world is one host — it recovers as a total crash: the whole world
    // restarts as ONE new host (a new id), the generation raised exactly once.
    let again = restart_alone(&mut w, &db, &dir, 3001);
    assert_eq!((again.generation, again.bumped), (generation + 1, true), "a total crash: + 1");
    assert_eq!(w.claims[0].store().world_generation().unwrap(), generation + 1, "raised exactly once");
    w.pair.until("the slime back in a running region", |p| {
        p.sims.iter_mut().map(|s| s.holders(id).len()).sum::<usize>() > 0
    });
    for _ in 0..40 {
        w.pair.round();
    }
    assert_eq!(holders_all(&mut w, id).len(), 1, "the slime once: {:?}", holders_all(&mut w, id));
    assert_eq!(prepared(&db), 0, "nothing stays prepared");
    w.stop(&dir);
}

/// A host whose `hosts` row was marked dead (a takeover declared it) learns it from its next
/// heartbeat — the lease answers "replaced" — and its loop ends fenced within 3 s: its
/// sessions refused, nothing more saved (its seam region's row stays as it was), nothing
/// left prepared. The other host's cuts do not commit without it (Elastic 1 Task 4): they
/// exchanged state since their last committed cut, and no succession resolved it.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn replaced_host_exits_fenced_at_its_next_heartbeat() {
    use server::lease::{pump_or_fence, Heartbeat};

    install_props();
    let Some(db) = PgDb::create("kreplaced") else { return };
    let dir = scratch("kreplaced");
    let mut w = world(&db, &dir);
    w.until_committed_after(&db, 0);
    // Host 1's main-loop lease step, as `server::run` takes it every pass.
    let fenced = w.claims[1].fenced().clone();
    let mut beat = Some(Heartbeat::new(w.claims[1].store().clone(), h(1), 1001));
    let live_until = Instant::now() + Duration::from_millis(game::HOST_HEARTBEAT_MS + 500);
    while Instant::now() < live_until {
        w.pair.round();
        assert!(!pump_or_fence(&mut beat, &fenced, Instant::now()), "a live row holds its lease");
    }
    assert!(!fenced.is_set());
    exec(&db, "UPDATE hosts SET state = 1 WHERE id = 1");
    let marked = Instant::now();
    loop {
        w.pair.round();
        if pump_or_fence(&mut beat, &fenced, Instant::now()) {
            break;
        }
        assert!(marked.elapsed() < Duration::from_secs(3), "no Replaced within 3 s of the row marked dead");
    }
    assert!(marked.elapsed() < Duration::from_secs(3));
    assert!(fenced.is_set(), "the lease step fenced the process");
    let b1 = seam_of(&w, h(1));
    let saved = |db: &PgDb| query_i64s(db, &format!("SELECT saved_tick FROM regions WHERE rx = {} AND rz = {}", b1.x, b1.z));
    let before = saved(&db);
    assert_eq!(exit_host(&mut w, h(1)), server::claims::ServerExit::Fenced);
    assert_eq!(saved(&db), before, "a replaced host saves nothing on its way out");
    // Elastic 1 Task 4: host 0's cuts do NOT go on. Host 1's dead row is no longer expected,
    // but host 0 exchanged state with host 1 since their last committed cut, and host 1 is in
    // none of the cuts now (marked dead by hand, under `Rollback`, with no succession — in the
    // product only a declaration marks a row dead, and its succession makes host 1 a
    // succeeded peer here): what crossed from host 1 may be stored in host 1's last part AND in
    // host 0's (the R3 version of this test pinned exactly that: entities stored twice). Host 0
    // refuses its part instead ("wait, never guess", `world_cut.rs` "Hosts outside the cut").
    // Host 0 may commit a cut alone right after host 1 is gone, while nothing unsaved stands
    // between them; once it holds unsaved traffic with host 1 (it sends toward its neighbour as
    // it runs) it refuses its parts. The claim is about that state: from the first refusal on,
    // no cut of host 0 alone commits. (How soon the traffic comes is timing, so `last` is read
    // once host 0 refused, not at host 1's exit.)
    w.pair.until("host 0 refuses a cut for its unsaved traffic with host 1", |p| {
        p.sims.iter().find(|s| s.ordinal == h(0)).and_then(|s| s.manager.world_cut_refused_for()).is_some_and(|(_, with)| with == [h(1)])
    });
    let (last, _) = last_committed(&db);
    let until = Instant::now() + 6 * CUT_EVERY;
    while Instant::now() < until {
        w.pair.round();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!decisions(&db).iter().any(|(c, _, k)| *k && *c > last), "no cut of host 0 alone commits beside unsaved traffic with host 1");
    assert!(decisions(&db).iter().any(|(c, _, k)| !*k && *c > last), "host 0 refused its parts: the cuts were aborted");
    // The cause is that rule, not another abort: host 0 refused for its unsaved traffic with host 1.
    let refused = w.pair.sims.iter().find(|s| s.ordinal == h(0)).and_then(|s| s.manager.world_cut_refused_for());
    assert!(
        refused.as_ref().is_some_and(|(c, with)| c.0 as i64 > last && with == &[h(1)]),
        "host 0 refused a cut after {last} for its unsaved traffic with host 1: {refused:?}"
    );
    // This hand-made state's outage is PERMANENT: no succession ever settles host 1 here (the
    // `Rollback` coordinator declares none), so host 0's cuts stay refused — in the product a
    // declaration's succession makes host 1 a succeeded peer and the cuts resume after its settle.
    assert_eq!(saved(&db), before, "nothing of host 1's region saved after it was replaced");
    assert!(entity_rows_stored_twice(&db).is_empty(), "no entity stored twice");
    assert_eq!(prepared(&db), 0, "nothing stays prepared");
    w.stop(&dir);
}

// `clean_leave_and_return_keeps_the_generation_and_delivers_waiting_transfers` moved (Elastic 1) to `takeover.rs` as `a_clean_leave_keeps_the_generation_and_its_waiting_transfers_reach_the_heir`: a cleanly
// departed host never returns, its land and the transfers waiting for it reach a new host only
// through a `Leave` succession, which this rig's `Rollback` coordinator never declares.

/// Final review I1, Elastic 1 Task 4 (Review Focus 4): host 1 leaves cleanly and its process
/// starts again — a NEW host owning nothing — while a cut of host 0 alone is pending (begun and
/// its begin recorded in the store, host 0's part not decided). The cut is NOT aborted: host 0
/// freezes its session with the newcomer until the decision (`world_cut.rs`, "Hosts outside the
/// cut"), the coordinator lets the attach pass, and the store lets the decision leave out a host
/// registered after the recorded begin. It commits with host 0's part only; the next cut takes
/// host 0 and the new host and commits; a slime of host 0's is held and stored once.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn a_new_host_registering_beside_a_cut_that_excludes_it_is_in_the_next_cut() {
    install_props();
    let Some(db) = PgDb::create("lback") else { return };
    let dir = scratch("lback");
    let mut w = world(&db, &dir);
    let generation = w.claims[0].store().world_generation().unwrap();
    w.until_committed_after(&db, 0);
    assert_eq!(exit_host(&mut w, h(1)), server::claims::ServerExit::Clean, "host 1 left");
    assert_eq!(query_i64s(&db, "SELECT clean FROM hosts WHERE id = 1"), [1]);
    let a0 = seam_of(&w, h(0));
    let id = spawn_slime_in(&mut w, h(0), a0, REGION_SIZE / 2.0);
    // A cut of host 0 alone begins, and the coordinator records its begin.
    let cut = until_part(&mut w, 0, &["pending", "marked"]);
    assert_eq!(decision_of(&db, cut), None);
    let begun = format!("SELECT COUNT(*) FROM cut_begins WHERE cut_id = {}", cut.0);
    w.pair.until("the cut's begin is recorded", |_| query_i64s(&db, &begun) == [1]);
    // Host 1's process starts again as a NEW host while the cut is pending.
    let again = restart_host(&mut w, &db, &dir, h(1), 3001);
    assert_eq!((again.generation, again.bumped), (generation, false), "a new host: the generation kept");
    assert!(query_i64s(&db, &format!("SELECT hosts_below FROM cut_begins WHERE cut_id = {}", cut.0))[0] <= again.id.0 as i64);
    w.pair.until("the cut that excludes the new host is decided", |_| decision_of(&db, cut).is_some());
    assert_eq!(decision_of(&db, cut), Some(true), "the cut of host 0 alone commits beside the newcomer");
    w.pair.until("the new host attached", |p| p.sims.iter().all(|s| s.manager.world_attached()));
    w.until_committed_after(&db, cut.0 as i64);
    let last: Vec<i64> = query_i64s(&db, "SELECT MAX(cut_id) FROM cut_decisions WHERE committed = 1");
    assert!(last[0] > cut.0 as i64, "a later cut committed");
    w.pair.until("the new host took part in a later committed cut", |p| {
        p.sims.iter().any(|s| s.ordinal == again.id && s.manager.peer_journal().base() > cut)
    });
    assert_eq!(holders_all(&mut w, id).len(), 1, "held once: {:?}", holders_all(&mut w, id));
    let (birth, serial) = (id.birth_region, id.serial);
    let stored = query_i64s(
        &db,
        &format!("SELECT COUNT(*) FROM entities WHERE birth_rx = {} AND birth_rz = {} AND serial = {serial}", birth.x, birth.z),
    );
    assert_eq!(stored, [1], "stored once");
    for h in (0..u64::from(HOSTS)).map(HostId) {
        assert!(!w.generations[h.0 as usize].is_set(), "no rollback");
    }
    assert_eq!(prepared(&db), 0, "nothing stays prepared");
    w.stop(&dir);
}

/// Final review I3: a multi-host stack killed with a part staged, then the same Postgres
/// world started as role `all`: its start-up resolves the orphan part before the claim,
/// which then proceeds (it would wait on the part's `super_regions` locks for ever).
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn role_all_on_postgres_resolves_an_orphan_part_before_its_claim() {
    install_props();
    let Some(db) = PgDb::create("allorph") else { return };
    let (store, claims) = claim(&db, h(0));
    let generation = store.world_generation().unwrap();
    let srs: Vec<_> = claims.fence().epochs.keys().copied().collect();
    let orphan = game::WorldCut { id: game::CutId(9), host: h(0), tick: 1, regions: vec![], in_flight: vec![], players: vec![], history: None, adopts: vec![] };
    store.prepare_cut(orphan, (*claims.fence()).clone(), generation).recv().unwrap().expect("staged");
    // The host process dies: the part stays prepared.
    drop(claims);
    store.shutdown();
    assert_eq!(prepared(&db), 1);
    // Role `all` on the same world.
    let all = db.open();
    server::roles::resolve_before_claim(true, &all, "").expect("the start-up resolves staged parts");
    let (tx, rx) = crossbeam::channel::bounded(1);
    let claimer = {
        let all = all.clone();
        std::thread::spawn(move || {
            let _ = tx.send(all.claim_super_regions(h(0), srs));
        })
    };
    let claimed = rx.recv_timeout(Duration::from_secs(10));
    if claimed.is_err() {
        // Let the stuck claim finish before failing: nothing stays prepared.
        db.open().resolve_prepared().unwrap();
    }
    claimer.join().unwrap();
    claimed.expect("the claim proceeds").expect("claimed");
    assert_eq!(prepared(&db), 0, "nothing stays prepared");
    all.shutdown();
}

/// A host of several exiting with no coordinator to cut for it waits its leave timeout
/// and exits 0 without a cut: its row stays dirty, so its return raises the generation.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn leave_without_a_coordinator_exits_dirty() {
    install_props();
    let Some(db) = PgDb::create("nocoord") else { return };
    let dir = scratch("nocoord");
    let (mut pair, _seam, mut links, claims) = two_hosts(&db, &dir);
    let generation = claims[1].store().world_generation().unwrap();
    let g1 = server::claims::WorldGeneration::new(generation);
    let mut cuts = server::cuts::CutWriter::with_world_cuts(claims[1].clone(), g1.clone());
    cuts.set_leave_timeout(Duration::from_secs(2));
    let mut host1 = pair.sims.remove(1);
    let region_out = host1.region_out.clone();
    let started = pair.started;
    let fenced = claims[1].fenced().clone();
    let begun = Instant::now();
    let exit = server::cuts::exit_world(
        &mut host1.manager,
        server::cuts::ExitParams {
            region_out: &region_out,
            now_ms: || started.elapsed().as_millis() as u64,
            cuts: Some(cuts),
            coordinator: None,
            fenced: &fenced,
            world_generation: &g1,
            park_timeout: server::cuts::EXIT_PARK_TIMEOUT,
            save_timeout: Duration::from_secs(10),
            now_tick: || 0,
        },
        |m| {
            m.spawner_mut().tick_all();
            m.spawner_mut().pump();
            pair.round();
        },
    );
    assert_eq!(exit, server::claims::ServerExit::Clean, "exits 0");
    assert!(begun.elapsed() >= Duration::from_secs(2), "after its leave timeout");
    assert_eq!(query_i64s(&db, "SELECT clean FROM hosts WHERE id = 1"), [0], "its row stays dirty");
    drop(host1);
    links.remove(1);
    // Elastic 1: its return under its name is a NEW host at once (host 0 runs: no total crash,
    // no rollback) owning nothing; its old row is lapsed by the name (declared at the
    // coordinator's next read, its land then inherited).
    let store1 = db.open();
    let (back, again) = boot_links(&store1, next_id(&db), "host-1", 4001, TOKEN, &dir, "127.0.0.1:0");
    assert_eq!((again.id, again.generation, again.bumped), (h(2), generation, false), "a new host, nothing raised");
    assert!(store1.owned_by(again.id).unwrap().epochs.is_empty(), "it owns nothing");
    assert_eq!(query_i64s(&db, "SELECT seen_ms FROM hosts WHERE id = 1"), [0], "its old row lapsed by its name");
    drop(back);
    drop(pair);
    drop(links);
    store1.shutdown();
    for c in claims {
        c.store().clone().shutdown();
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Host 1 leaves; host 0's store fails to stage its part of the leave cut (one write):
/// `exit_world` sees host 1's part settled aborted, asks again, and the second leave cut
/// commits — host 1 exits clean with its row clean.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn aborted_leave_cut_is_asked_for_again_and_commits() {
    install_props();
    let Some(db) = PgDb::create("lretry") else { return };
    let dir = scratch("lretry");
    let mut w = world(&db, &dir);
    w.until_committed_after(&db, 0);
    let mut first = None;
    let exit = exit_host_with(&mut w, h(1), None, |m, claims| {
        if first.is_none() {
            if let Some(c) = m.leaving_cut() {
                first = Some(c);
                claims[0].store().fail_next_writes(1);
            }
        }
    });
    assert_eq!(exit, server::claims::ServerExit::Clean, "host 1 left");
    let first = first.expect("a leave cut began");
    assert_eq!(decision_of(&db, first), Some(false), "the first leave cut was aborted");
    assert!(decisions(&db).iter().any(|(c, _, k)| *k && *c as u64 > first.0), "a later leave cut committed");
    assert_eq!(query_i64s(&db, "SELECT clean FROM hosts WHERE id = 1"), [1], "host 1's row is clean");
    assert_eq!(prepared(&db), 0, "nothing stays prepared");
    w.stop(&dir);
}

/// The leave timeout of [`aborted_leave_cut_past_the_leave_timeout_exits_dirty`]: long
/// enough for its first leave cut to begin within it.
const PAST_LEAVE_TIMEOUT: Duration = Duration::from_secs(3);

/// The same failed leave cut with a leave timeout already past when it is aborted: host 1
/// exits without a cut, its row dirty. Deterministic in its timing: once the first leave
/// cut began (within the timeout), the exiting host's loop is held in its idle step until
/// the timeout has passed since its leave request — nothing of either host runs meanwhile —
/// so that cut's abort can only settle after it.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn aborted_leave_cut_past_the_leave_timeout_exits_dirty() {
    install_props();
    let Some(db) = PgDb::create("lpast") else { return };
    let dir = scratch("lpast");
    let mut w = world(&db, &dir);
    w.until_committed_after(&db, 0);
    let mut first = None;
    // The first idle step runs right after the leave request: no earlier than it.
    let mut asked: Option<Instant> = None;
    let exit = exit_host_with(&mut w, h(1), Some(PAST_LEAVE_TIMEOUT), |m, claims| {
        let asked = *asked.get_or_insert_with(Instant::now);
        if first.is_none() {
            if let Some(c) = m.leaving_cut() {
                assert!(asked.elapsed() < PAST_LEAVE_TIMEOUT, "the first leave cut began within the timeout");
                first = Some(c);
                claims[0].store().fail_next_writes(1);
                std::thread::sleep((asked + PAST_LEAVE_TIMEOUT).saturating_duration_since(Instant::now()));
            }
        }
    });
    assert_eq!(exit, server::claims::ServerExit::Clean, "exits 0");
    let first = first.expect("a leave cut began within the timeout");
    assert_eq!(decision_of(&db, first), Some(false), "the leave cut was aborted");
    assert!(!decisions(&db).iter().any(|(c, _, k)| *k && *c as u64 > first.0), "no later cut");
    assert_eq!(query_i64s(&db, "SELECT clean FROM hosts WHERE id = 1"), [0], "its row stays dirty");
    assert_eq!(prepared(&db), 0, "nothing stays prepared");
    w.stop(&dir);
}


// `both_hosts_stopped_together_leave_cleanly` moved (Elastic 1) to `takeover.rs` as `both_hosts_stopped_together_leave_cleanly_and_new_hosts_inherit_by_leave_successions`: a cleanly
// departed host never returns, its land and the transfers waiting for it reach a new host only
// through a `Leave` succession, which this rig's `Rollback` coordinator never declares.

/// Task 14b finding (fixed by Task 12b): a slime stored in host 0's seam region by one
/// committed cut crosses into host 1's region before the next. Host 0's part deletes its
/// region's `entities` rows and host 1's part inserts into its own region's; with one key
/// per entity world-wide that was two prepared transactions on one row — host 1 waited for
/// host 0's prepared part until its lock timeout (or hit a unique violation if it went
/// first), the cut aborted, and so did every later one: world saving stalled for good.
/// Rows are keyed per region now: the later cut commits, the slime stored once.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn entity_crossing_hosts_between_cuts_does_not_stall_saving() {
    install_props();
    let Some(db) = PgDb::create("xcross") else { return };
    let dir = scratch("xcross");
    let mut w = world(&db, &dir);
    let a0 = seam_of(&w, h(0));
    let id = spawn_slime_in(&mut w, h(0), a0, REGION_SIZE / 2.0);
    w.until_committed_after(&db, 0);
    let (first, _) = last_committed(&db);
    w.until_committed_after(&db, first);
    let (stored, _) = last_committed(&db);
    let b1 = seam_of(&w, h(1));
    let rows = |column: &str| {
        format!(
            "SELECT {column} FROM entities WHERE birth_rx = {} AND birth_rz = {} AND serial = {}",
            id.birth_region.x, id.birth_region.z, id.serial as i64
        )
    };
    let stored_in = || -> Vec<(i64, i64)> { query_i64s(&db, &rows("rx")).into_iter().zip(query_i64s(&db, &rows("rz"))).collect() };
    assert_eq!(stored_in(), [(i64::from(a0.x), i64::from(a0.z))], "the earlier cut stores the slime once, in host 0's region");
    push_over_the_seam(&mut w, h(0), id);
    w.pair.until("the slime on host 1", |p| p.sims[1].holders(id) == [b1]);
    w.until_committed_after(&db, stored);
    assert_eq!(stored_in(), [(i64::from(b1.x), i64::from(b1.z))], "the later cut stores the slime once, in host 1's region");
    assert_eq!(prepared(&db), 0, "nothing stays prepared");
    w.stop(&dir);
}

// ---- the login relay (Task 15) ------------------------------------------------------------

/// The login service's end of the entry host's login link.
struct Login {
    tx: crossbeam::channel::Sender<LoginToHost>,
    events: Receiver<svc_link::LinkEvent<HostToLogin>>,
    /// Everything the host sent so far, in order.
    seen: Vec<HostToLogin>,
}

impl Login {
    /// What arrived since the last look, appended to `seen`.
    fn drain(&mut self) {
        for ev in self.events.try_iter() {
            if let svc_link::LinkEvent::Msg(msg) = ev {
                self.seen.push(msg);
            }
        }
    }
}

/// The players by token, with their stored records (a test's player store).
type Roster = Arc<std::sync::Mutex<std::collections::BTreeMap<String, (PlayerId, Option<PlayerRecord>)>>>;

/// A player store over a [`Roster`]: an unknown token is an error.
struct RosterPlayers(Roster);

impl PlayerStore for RosterPlayers {
    fn resolve(&mut self, key: &PlayerKey) -> std::io::Result<(PlayerId, Option<PlayerRecord>)> {
        let PlayerKey::Online(token) = key else { return Err(std::io::Error::other("not a login")) };
        self.0.lock().unwrap().get(&token.0).cloned().ok_or_else(|| std::io::Error::other("an unknown token"))
    }
}

/// Two hosts and a real coordinator ([`world`]) whose login relays run, the login service
/// dialed in to the COORDINATOR's login listener (Elastic 1 Task 6: it relays each order to the
/// gate host, the lowest attached live one — host 0 — and the answers back), over the players
/// of `roster`.
struct LoginWorld {
    w: World,
    /// Where the login service dials (the coordinator's login listener) and its pin.
    login_at: (std::net::SocketAddr, String),
    login: Login,
    roster: Roster,
}

/// The login service's link to the coordinator at `at` (its pin), up.
fn dial_login(at: &(std::net::SocketAddr, String)) -> Login {
    let hello = svc_link::LinkHello::new(TOKEN.into(), svc_link::PeerRole::Login, RIG_WORLD);
    let (tx, events) = svc_link::LinkDialer::connect::<HostToLogin, LoginToHost>(at.0, at.1.clone(), hello);
    match events.recv_timeout(WAIT) {
        Ok(svc_link::LinkEvent::Up(_)) => {}
        other => panic!("the login link did not come up: {other:?}"),
    }
    Login { tx, events, seen: Vec::new() }
}

fn login_hosts(db: &PgDb, dir: &std::path::Path) -> LoginWorld {
    let mut w = world(db, dir);
    let roster: Roster = Arc::default();
    let players = server::player_store::shared(RosterPlayers(roster.clone()));
    for sim in &mut w.pair.sims {
        let (events_tx, events) = unbounded();
        let orders = Arc::new(SpawnOrders::new(server::auth_edge::system_clock()));
        let relay = LoginRelay::new(orders.clone(), players.clone(), events_tx);
        sim.login = Some(SimLogin { orders, relay, events });
    }
    let login_at = w.coordinator.as_ref().unwrap().login.clone().expect("the coordinator listens for the login service");
    let login = dial_login(&login_at);
    // Each host's first sync of the other is done (its `Running` came).
    w.pair.until("every peer synced", |p| p.sims.iter().all(|s| s.manager.peers_synced()));
    LoginWorld { w, login_at, login, roster }
}

impl LoginWorld {
    fn stop(self, dir: &std::path::Path) {
        drop(self.login);
        self.w.stop(dir);
    }

    /// A login token of `player` whose stored record stands in the middle of `rc`
    /// (`None`: no record).
    fn player(&self, n: u8, player: PlayerId, rc: Option<RegionCoords>) -> String {
        let token = game::PlayerToken::generate([n; 16]).0;
        let record = rc.map(|rc| PlayerRecord {
            client: game::Client::default(),
            pose: game::Pose::from_translation(game::rapier3d::math::Vector::new(
                REGION_SIZE / 2.0,
                above_ground_y(),
                REGION_SIZE / 2.0,
            )),
            region: rc,
            adventure: None,
        });
        self.roster.lock().unwrap().insert(token.clone(), (player, record));
        token
    }

    fn sim(&mut self, h: HostId) -> &mut Sim {
        self.w.pair.sims.iter_mut().find(|s| s.ordinal == h).expect("the host is up")
    }

    fn orders(&mut self, h: HostId) -> Arc<SpawnOrders> {
        self.sim(h).login.as_ref().unwrap().orders.clone()
    }

    /// Rounds until the login service has received a message `want` holds of (counting
    /// from `from` in `seen`); its index.
    fn until_login(&mut self, what: &str, from: usize, want: impl Fn(&HostToLogin) -> bool) -> usize {
        let deadline = Instant::now() + WAIT;
        loop {
            self.login.drain();
            if let Some(i) = self.login.seen.iter().skip(from).position(&want) {
                return from + i;
            }
            assert!(Instant::now() < deadline, "timed out waiting for the login service to hear: {what} (seen {:?})", self.login.seen);
            self.w.pair.round();
        }
    }

    /// `rounds` rounds, the login service listening.
    fn rounds(&mut self, rounds: usize) {
        for _ in 0..rounds {
            self.w.pair.round();
            self.login.drain();
        }
    }

    /// The login service orders a spawn of `token` at `epoch` and waits for the ack.
    fn join(&mut self, token: &str, epoch: u64) -> HostToLogin {
        let from = self.login.seen.len();
        self.login.tx.send(LoginToHost::SpawnPlayer { player_token: token.into(), session_epoch: epoch }).unwrap();
        let i = self.until_login("the spawn ack", from, |m| {
            matches!(m, HostToLogin::SpawnAck { player_token, session_epoch, .. } if player_token == token && *session_epoch == epoch)
        });
        self.login.seen[i].clone()
    }

    /// The client of the ticket (`token`, `epoch`) connects to host `h`, as its edge admits
    /// and announces it; its session id.
    fn connect(&mut self, h: HostId, token: &str, epoch: u64) -> ClientId {
        let (player, record) = self.roster.lock().unwrap()[token].clone();
        let orders = self.orders(h);
        orders.admit(token, epoch).expect("the ticket is admitted");
        let client = game::session_client_id(player, epoch);
        let now = self.w.pair.now();
        let manager = &mut self.sim(h).manager;
        let ev = ServerEvent::LoginConnected { client, player, record, player_token: token.into(), session_epoch: epoch };
        orders.announce(token, epoch, || assert!(manager.handle_server_event(ev, now))).expect("announced");
        client
    }

    /// The client's connection to host `h` closes, as its edge reports it.
    fn disconnect(&mut self, h: HostId, client: ClientId, token: &str, epoch: u64) {
        let now = self.w.pair.now();
        assert!(self.sim(h).manager.handle_server_event(ServerEvent::ClientDisconnected(client), now));
        self.orders(h).closed(token, epoch);
    }

    /// Where `client`'s body is: `(host, region)` of every running region holding it.
    fn bodies(&mut self, client: ClientId) -> Vec<(HostId, RegionCoords)> {
        let mut out = Vec::new();
        for sim in &mut self.w.pair.sims {
            for rc in sim.manager.running_regions() {
                sim.manager.spawner_mut().with_region(rc, |region| {
                    if region.data().player_entites.contains_key(&client) {
                        out.push((sim.ordinal, rc));
                    }
                });
            }
        }
        out
    }

    /// Push `client`'s body (in `from`, on host `h`) past the seam into `to`.
    fn push_across(&mut self, h: HostId, client: ClientId, from: RegionCoords, to: RegionCoords) {
        let y = above_ground_y();
        let past = |d: i32| match d {
            1 => REGION_SIZE + game::FLIP_HYSTERESIS + 2.0,
            -1 => -game::FLIP_HYSTERESIS - 2.0,
            _ => REGION_SIZE / 2.0,
        };
        let at = game::rapier3d::math::Vector::new(past(to.x - from.x), y, past(to.z - from.z));
        let mut pushed = false;
        self.sim(h).manager.spawner_mut().with_region(from, |region| {
            if let Some(&key) = region.data().player_entites.get(&client) {
                region.with_data(|d| d.set_body_pose_safe(key, game::Pose::from_translation(at)));
                pushed = true;
            }
        });
        assert!(pushed, "{from:?} on host {h} holds client {client}'s body");
    }

    /// The SessionEnded messages for (`token`, `epoch`) the login service heard.
    fn ended(&self, token: &str, epoch: u64) -> usize {
        let want = HostToLogin::SessionEnded { player_token: token.into(), session_epoch: epoch };
        self.login.seen.iter().filter(|m| **m == want).count()
    }
}

/// `(the region host h owns, the other)` of the seam.
fn sides(seam: (RegionCoords, RegionCoords), h: HostId, w: &LoginWorld) -> (RegionCoords, RegionCoords) {
    if w.w.pair.sims[0].manager.owner_host(seam.0) == Some(h) { (seam.0, seam.1) } else { (seam.1, seam.0) }
}

fn ack_quic(ack: &HostToLogin) -> (bool, Option<String>) {
    match ack {
        HostToLogin::SpawnAck { ok, quic, .. } => (*ok, quic.clone()),
        other => panic!("not an ack: {other:?}"),
    }
}

/// A login whose stored record lies in host 1's region: the entry host (0) relays the
/// order, host 1 takes it, and the ack names host 1's address. Host 0 holds no order.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn join_places_the_player_on_the_host_owning_its_record_region() {
    install_props();
    let Some(db) = PgDb::create("ljoin") else { return };
    let dir = scratch("ljoin");
    let mut w = login_hosts(&db, &dir);
    let (b1, _) = sides(w.w.seam, h(1), &w);
    let token = w.player(1, PlayerId(900), Some(b1));
    let ack = w.join(&token, 1);
    assert_eq!(ack_quic(&ack), (true, endpoint(h(1)).quic), "host 1 took it");
    assert!(w.orders(h(1)).pending().by_token.contains_key(&token), "host 1 holds the pending spawn");
    assert!(w.orders(h(0)).pending().by_token.is_empty(), "host 0 holds none");
    // The client connects there and plays there.
    let client = w.connect(h(1), &token, 1);
    let deadline = Instant::now() + WAIT;
    while w.bodies(client).is_empty() {
        assert!(Instant::now() < deadline, "the player never got a body");
        w.rounds(1);
    }
    assert_eq!(w.bodies(client), [(h(1), b1)]);
    w.stop(&dir);
}

/// The player walked from host 0 to host 1 and its client disconnected; no cut since (its
/// stored record still in host 0's region). A new login follows host 0's `departed`
/// pointer to host 1, lands there, and the player has one body.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn rejoin_within_a_cut_follows_the_departed_pointer() {
    install_props();
    let Some(db) = PgDb::create("lrejoin") else { return };
    let dir = scratch("lrejoin");
    let mut w = login_hosts(&db, &dir);
    let (a0, b1) = sides(w.w.seam, h(0), &w);
    let token = w.player(2, PlayerId(901), Some(a0));
    assert_eq!(ack_quic(&w.join(&token, 1)), (true, endpoint(h(0)).quic));
    let c1 = w.connect(h(0), &token, 1);
    let deadline = Instant::now() + WAIT;
    while w.bodies(c1).is_empty() {
        assert!(Instant::now() < deadline, "the player never got a body");
        w.rounds(1);
    }
    w.push_across(h(0), c1, a0, b1);
    let deadline = Instant::now() + WAIT;
    while w.bodies(c1) != [(h(1), b1)] || w.w.pair.sims.iter().any(|s| s.manager.peer_unreceived() > 0) {
        assert!(Instant::now() < deadline, "the player never crossed: {:?}", w.bodies(c1));
        w.rounds(1);
    }
    assert_eq!(w.sim(h(0)).manager.departed().get(&PlayerId(901)), Some(&h(1)));
    w.disconnect(h(0), c1, &token, 1);
    let ack = w.join(&token, 2);
    assert_eq!(ack_quic(&ack), (true, endpoint(h(1)).quic), "the login lands on host 1");
    let c2 = w.connect(h(1), &token, 2);
    let deadline = Instant::now() + WAIT;
    while w.bodies(c2).is_empty() {
        assert!(Instant::now() < deadline, "the new login never got a body");
        w.rounds(1);
    }
    w.rounds(20);
    assert_eq!(w.bodies(c2), [(h(1), b1)], "one body, on host 1");
    assert!(w.bodies(c1).is_empty(), "the old session's body is gone");
    for h in (0..u64::from(HOSTS)).map(HostId) {
        assert!(w.sim(h).manager.parked_record(PlayerId(901)).is_none(), "no parked copy on host {h}");
    }
    w.stop(&dir);
}

/// A session on host 1 (not the entry host): the login service's kick, sent to host 0,
/// parks it there, and is acknowledged once.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn kick_reaches_a_session_on_a_non_entry_host_and_is_acked_once() {
    install_props();
    let Some(db) = PgDb::create("lkick") else { return };
    let dir = scratch("lkick");
    let mut w = login_hosts(&db, &dir);
    let (b1, _) = sides(w.w.seam, h(1), &w);
    let token = w.player(3, PlayerId(902), Some(b1));
    assert_eq!(ack_quic(&w.join(&token, 1)), (true, endpoint(h(1)).quic));
    let client = w.connect(h(1), &token, 1);
    let deadline = Instant::now() + WAIT;
    while w.bodies(client).is_empty() {
        assert!(Instant::now() < deadline, "the player never got a body");
        w.rounds(1);
    }
    let from = w.login.seen.len();
    w.login.tx.send(LoginToHost::Kick { player_token: token.clone(), session_epoch: 1, reason: "signed out".into() }).unwrap();
    let ack = HostToLogin::KickAck { player_token: token.clone(), session_epoch: 1 };
    w.until_login("the kick ack", from, |m| *m == ack);
    w.rounds(20);
    assert_eq!(w.login.seen[from..].iter().filter(|m| **m == ack).count(), 1, "acknowledged once");
    assert!(w.bodies(client).is_empty(), "the session's body is gone");
    assert!(w.sim(h(1)).manager.parked_record(PlayerId(902)).is_some(), "parked on host 1");
    assert_eq!(w.orders(h(1)).revoked(&token, 1).as_deref(), Some("signed out"), "host 1 revoked the login");
    assert_eq!(w.ended(&token, 1), 1, "the kicked session's end reached the login service once");
    w.stop(&dir);
}

/// The host holding a player's record region cannot be reached: the gate host refuses the
/// spawn order (the login service answers 503 `world_unavailable`).
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn spawn_for_an_unreachable_owner_is_refused() {
    install_props();
    let Some(db) = PgDb::create("lunreach") else { return };
    let dir = scratch("lunreach");
    let mut w = login_hosts(&db, &dir);
    let (b1, _) = sides(w.w.seam, h(1), &w);
    let token = w.player(4, PlayerId(903), Some(b1));
    // Host 1 is gone: its manager, its links and its coordinator link.
    kill_host(&mut w.w, h(1));
    w.w.pair.until("host 0 no longer reaches host 1", |p| !p.sims[0].manager.reaches(b1));
    // The login service itself, over its own link to the coordinator, which relays to the
    // gate (host 0): the gate refuses and the join answers 503 — on the refusal, well before
    // the ack timeout (not by timing out on a link that never came up: `login_join` asserts
    // the coordinator's login listener received the order).
    let (status, code, took) = login_join(&mut w, &token);
    assert_eq!(status, 503);
    assert_eq!(code, login_api::ApiErrorCode::WorldUnavailable);
    assert!(
        took < Duration::from_millis(login_api::SPAWN_ACK_TIMEOUT_MS),
        "answered by the gate's refusal, not the ack timeout ({took:?})"
    );
    assert!(w.orders(h(0)).pending().by_token.is_empty(), "nothing ordered on host 0");
    // The same, hand-dialled at the coordinator (a later epoch): refused, nothing ordered.
    let ack = w.join(&token, 2);
    assert_eq!(ack_quic(&ack), (false, None), "refused");
    assert!(w.orders(h(0)).pending().by_token.is_empty(), "nothing ordered on host 0");
    w.stop(&dir);
}

/// A real login service (its HTTP API in-process, an in-memory store, its own link to the
/// coordinator's login listener, its hello naming the registry row's world — the rig's
/// [`RIG_WORLD`], so it is admitted) joins the account that owns `token` to the world while
/// the hosts run round by round: the join's HTTP status and body, and how long it took. It
/// asserts the coordinator's login listener received the order (the link really came up:
/// a join must never pass by timing out on a link that was refused).
fn login_join_raw(w: &mut LoginWorld, token: &str) -> (u16, Vec<u8>, Duration) {
    use tower::ServiceExt;
    const WORLD: &str = RIG_WORLD;
    let orders_before = w.w.coordinator.as_ref().unwrap().login_counts().expect("a login listener").orders;
    let started = Instant::now();
    const NOW: u64 = 1_800_000_000;
    let (addr, pin) = (w.login_at.0.to_string(), w.login_at.1.clone());
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    let token = token.to_string();
    let joined = rt.spawn(async move {
        let store = login::LoginStore::open("sqlite::memory:").await.unwrap();
        store
            .put_world(&login::WorldRow {
                world_id: WORLD.into(),
                name: "Hosts".into(),
                status: login_api::WorldStatus::Open,
                quic: Some("world-row.example:6466".into()),
                webtransport: None,
                cert_hash: None,
                created_at: 1,
                host_link: Some(addr),
                host_link_cert: Some(pin),
            })
            .await
            .unwrap();
        let account = store.ensure_verified_account("dwarf@example.org", 1).await.unwrap();
        assert_eq!(store.player_token_for(account, WORLD, 1, move || token.clone()).await.unwrap().len(), 32);
        let refresh = "hosts-test-refresh-token";
        let expires = (NOW + 3600) as i64;
        store.insert_refresh(login::api::token_hash(refresh), account, [7; 16], expires).await.unwrap();
        let clock: Arc<dyn Fn() -> u64 + Send + Sync> = Arc::new(|| NOW);
        let (key, _) = auth::SigningKey::generate();
        let state = login::AppState::new(
            store.clone(),
            Arc::new(auth::Signer::new(vec![key]).ok().unwrap()),
            Arc::new(login::LoginConfig::from_lookup(|_| None).unwrap()),
            clock.clone(),
        )
        .with_host_links(login::host_link::HostLinks::new(Some(TOKEN.into()), store, clock));
        let body = serde_json::to_vec(&login_api::JoinRequest { refresh_token: refresh.into(), world: WORLD.into() }).unwrap();
        let req = axum::http::Request::post(login_api::PATH_JOIN)
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(axum::body::Body::from(body))
            .unwrap();
        let resp = login::router(state).oneshot(req).await.unwrap();
        let status = resp.status().as_u16();
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20).await.unwrap();
        (status, bytes.to_vec())
    });
    let deadline = Instant::now() + WAIT;
    while !joined.is_finished() {
        assert!(Instant::now() < deadline, "the join never answered");
        w.rounds(1);
    }
    let (status, bytes) = rt.block_on(joined).unwrap();
    let took = started.elapsed();
    drop(rt);
    let counts = w.w.coordinator.as_ref().unwrap().login_counts().unwrap();
    assert!(counts.orders > orders_before, "the coordinator's login listener received the login service's order: {counts:?}");
    (status, bytes, took)
}

/// [`login_join_raw`] that failed: its HTTP status and error code, and how long it took.
fn login_join(w: &mut LoginWorld, token: &str) -> (u16, login_api::ApiErrorCode, Duration) {
    let (status, bytes, took) = login_join_raw(w, token);
    let err: login_api::ApiError = serde_json::from_slice(&bytes).unwrap_or_else(|e| panic!("not an error body ({e}): HTTP {status}, {} bytes", bytes.len()));
    (status, err.code, took)
}

/// The positive counterpart of `spawn_for_an_unreachable_owner_is_refused`: the real login
/// service's HTTP join through the coordinator relay succeeds (200) with a ticket naming the
/// host that took the order (the gate, host 0: the player's record is in its land).
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn a_real_login_join_through_the_coordinator_names_the_placing_host() {
    install_props();
    let Some(db) = PgDb::create("lhttp") else { return };
    let dir = scratch("lhttp");
    let mut w = login_hosts(&db, &dir);
    let (a0, _) = sides(w.w.seam, h(0), &w);
    let token = w.player(9, PlayerId(912), Some(a0));
    let (status, bytes, _) = login_join_raw(&mut w, &token);
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&bytes));
    let joined: login_api::JoinResponse = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(joined.ticket.quic, endpoint(h(0)).quic, "the ticket names host 0, not the world row");
    assert!(w.orders(h(0)).pending().by_token.contains_key(&token), "host 0 holds the pending spawn");
    w.stop(&dir);
}

/// A host that left cleanly (a committed leave cut, its row clean) is what a booting host
/// reads as left (`peer_link::left_cleanly`, seeding `note_left_peers`). Elastic 1: it never
/// comes back — its restarted process is a new host, which reads the clean row too; a crashed
/// host (its row dirty) is never left.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn hosts_that_left_cleanly_are_read_at_boot() {
    install_props();
    let Some(db) = PgDb::create("lclean") else { return };
    let dir = scratch("lclean");
    let mut w = world(&db, &dir);
    let store0 = w.claims[0].store().clone();
    assert_eq!(server::peer_link::left_cleanly(&store0, h(0)).unwrap(), [], "both run");
    w.until_committed_after(&db, 0);
    assert_eq!(exit_host(&mut w, h(1)), server::claims::ServerExit::Clean, "host 1 left");
    assert_eq!(server::peer_link::left_cleanly(&store0, h(0)).unwrap(), [(h(1), 1001)], "host 1 left as boot 1001");
    assert_eq!(server::peer_link::left_cleanly(&store0, h(1)).unwrap(), [], "its own row is not a peer's");
    // Elastic 1: a leaver never returns — its process starts again as a NEW host, the clean row
    // stays, and the new host reads it at its start; the new host's own row is dirty.
    let again = restart_host(&mut w, &db, &dir, h(1), 3001);
    assert_eq!(again.generation, w.claims[0].store().world_generation().unwrap(), "the generation kept");
    assert_eq!(server::peer_link::left_cleanly(&store0, h(0)).unwrap(), [(h(1), 1001)], "the clean row stays");
    assert_eq!(server::peer_link::left_cleanly(&store0, again.id).unwrap(), [(h(1), 1001)], "the new host reads it");
    kill_host(&mut w, again.id);
    assert_eq!(server::peer_link::left_cleanly(&store0, h(0)).unwrap(), [(h(1), 1001)], "a crashed host's row is dirty: not left");
    assert_eq!(prepared(&db), 0, "nothing stays prepared");
    w.stop(&dir);
}

/// A session on host 1 (not the gate host) disconnects: its end reaches the login service once,
/// up host 1's own coordinator link (Elastic 1 Task 6), relayed by the coordinator.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn session_ended_on_a_non_entry_host_reaches_login() {
    install_props();
    let Some(db) = PgDb::create("lended") else { return };
    let dir = scratch("lended");
    let mut w = login_hosts(&db, &dir);
    let (b1, _) = sides(w.w.seam, h(1), &w);
    let token = w.player(5, PlayerId(904), Some(b1));
    assert_eq!(ack_quic(&w.join(&token, 1)), (true, endpoint(h(1)).quic));
    let client = w.connect(h(1), &token, 1);
    let deadline = Instant::now() + WAIT;
    while w.bodies(client).is_empty() {
        assert!(Instant::now() < deadline, "the player never got a body");
        w.rounds(1);
    }
    assert_eq!(w.ended(&token, 1), 0);
    w.disconnect(h(1), client, &token, 1);
    let want = HostToLogin::SessionEnded { player_token: token.clone(), session_epoch: 1 };
    w.until_login("the session's end", 0, |m| *m == want);
    w.rounds(20);
    assert_eq!(w.ended(&token, 1), 1, "once");
    w.stop(&dir);
}

/// A player handed from host 0 to host 1 is still in the world: host 0 does not report its
/// session ended — not at the hand-off, not when the client's connection to host 0 closes
/// — so a kick the login service holds for it (queued before the crossing, resent now)
/// still parks the player on host 1. Its end is reported once, when that park takes it out
/// of the world.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn handoff_does_not_end_the_login_session() {
    install_props();
    let Some(db) = PgDb::create("lhandoff") else { return };
    let dir = scratch("lhandoff");
    let mut w = login_hosts(&db, &dir);
    let (a0, b1) = sides(w.w.seam, h(0), &w);
    let token = w.player(6, PlayerId(905), Some(a0));
    assert_eq!(ack_quic(&w.join(&token, 1)), (true, endpoint(h(0)).quic));
    let client = w.connect(h(0), &token, 1);
    let deadline = Instant::now() + WAIT;
    while w.bodies(client).is_empty() {
        assert!(Instant::now() < deadline, "the player never got a body");
        w.rounds(1);
    }
    w.push_across(h(0), client, a0, b1);
    let deadline = Instant::now() + WAIT;
    while w.bodies(client) != [(h(1), b1)] || w.w.pair.sims.iter().any(|s| s.manager.peer_unreceived() > 0) {
        assert!(Instant::now() < deadline, "the player never crossed: {:?}", w.bodies(client));
        w.rounds(1);
    }
    w.disconnect(h(0), client, &token, 1);
    w.rounds(10);
    assert_eq!(w.ended(&token, 1), 0, "a hand-off and a closed connection to the host it left end nothing");
    let from = w.login.seen.len();
    w.login.tx.send(LoginToHost::Kick { player_token: token.clone(), session_epoch: 1, reason: "signed out".into() }).unwrap();
    let ack = HostToLogin::KickAck { player_token: token.clone(), session_epoch: 1 };
    w.until_login("the kick ack", from, |m| *m == ack);
    w.rounds(20);
    assert!(w.bodies(client).is_empty(), "the kick parked the player");
    assert!(w.sim(h(1)).manager.parked_record(PlayerId(905)).is_some(), "on host 1");
    assert_eq!(w.ended(&token, 1), 1, "its end reported once, by the park");
    w.stop(&dir);
}

// ---- other hosts' saved terrain (Task 19) ------------------------------------------------

/// Block columns along a region's edge.
const REGION_EDGE: i32 = game::REGION_BLOCKS as i32;

/// The block at world block `p` as region `rc` of host `h` sees it (its own or its halo's):
/// `None` for air, or outside own ∪ halo.
fn seen_from(p: &mut Pair, h: usize, rc: RegionCoords, at: game::WorldBlockPos) -> Option<game::BlockId> {
    let mut seen = None;
    p.sims[h].manager.spawner_mut().with_region(rc, |r| seen = Some(r.data().halo_block(at)));
    seen.expect("the region runs").map(|b| b.id).filter(|id| *id != game::BlockId::AIR)
}

/// Host 0 digs at the border of its seam region while the peer link is down (the dig's
/// halo delta, the region's final halo and its `RegionParked` are all lost with the link),
/// then parks the region. Once a world cut has saved the park, host 1 hears the cut's
/// notice (`LISTEN`) and its running seam region's halo of the parked one is re-seeded from
/// the store: the dig shows there.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn edit_saved_by_host_0_reaches_host_1s_halo_after_the_cut() {
    install_props();
    let Some(db) = PgDb::create("terrain") else { return };
    let dir = scratch("terrain");
    let mut w = world(&db, &dir);
    let (a, b) = w.seam;
    let (ha, hb) = (w.pair.sims[0].manager.owner_host(a).expect("an owned region").0 as usize, w.pair.sims[0].manager.owner_host(b).expect("an owned region").0 as usize);
    for sim in &mut w.pair.sims {
        let store = w.claims[sim.ordinal.0 as usize].store().clone();
        sim.terrain = Some(server::cuts::TerrainWatch::start(store, world_store::NotifyMode::Listen));
    }
    w.pair.until("running sets exchanged", |p| {
        p.sims[ha].manager.foreign_running().contains(&b) && p.sims[hb].manager.foreign_running().contains(&a)
    });
    // The top block of `a`'s ground near its edge facing `b` (`b` is +x of `a`), and that
    // block as `b` sees it (its halo).
    let dig = [REGION_EDGE - 2, GROUND_BLOCKS as i32 - 1, 10];
    let at = game::WorldBlockPos::from_local(a, game::BlockPos::new(dig[0], dig[1], dig[2]));
    let seen_from_b = |p: &mut Pair| seen_from(p, hb, b, at);
    assert!(seen_from_b(&mut w.pair).is_some(), "b's halo holds a's ground");

    // The peer link drops.
    for sim in &w.pair.sims {
        sim.gate.set_down(true);
    }
    w.pair.until("the link is seen down", |p| !p.sims[hb].manager.foreign_running().contains(&a));
    // Host 0 digs at the border and parks the region.
    let mut dug_at = 0;
    w.pair.sims[ha].manager.spawner_mut().with_region(a, |r| {
        dug_at = r.current_tick() as i64;
        r.with_data(|d| assert!(d.set_block_safe(dig, game::Block::new(game::BlockId::AIR)), "the dig"));
    });
    // Since R4 Task 12 a world cut commits with the link down (counted across it,
    // `world_cut.rs`): once one holding the dig does, the store brings the dig to b's halo
    // with the link still down. Before that the dig's halo traffic is lost with the link —
    // checked at every look taken before any cut after the dig committed (the first one now:
    // no cut past the dig can have committed yet).
    let committed_past_dig = |db: &PgDb| decisions(db).iter().any(|(_, t, k)| *k && *t > dug_at);
    assert!(!committed_past_dig(&db), "no cut past the dig yet");
    assert!(seen_from_b(&mut w.pair).is_some(), "the dig's halo delta was lost with the link");
    let now = w.pair.now();
    assert!(w.pair.sims[ha].manager.handle_server_event(ServerEvent::ClientDisconnected(KEEPER), now));
    w.pair.until("a parked", |p| !p.sims[ha].manager.running_regions().contains(&a));
    for _ in 0..10 {
        if committed_past_dig(&db) {
            break;
        }
        assert!(seen_from_b(&mut w.pair).is_some(), "the dig's halo traffic and a's park were lost with the link");
        w.pair.round();
    }
    let parked_after = decisions(&db).iter().filter(|(_, t, _)| *t <= dug_at).map(|(c, _, _)| *c).max().unwrap_or(0);

    // The link comes back; nothing on it carries a's parked terrain (a does not run).
    for sim in &w.pair.sims {
        sim.gate.set_down(false);
    }
    w.pair.until("the dig reaches b's halo from the store", |p| seen_from_b(p).is_none());
    assert!(
        decisions(&db).iter().any(|(c, _, k)| *k && *c > parked_after),
        "brought by a world cut committed after the park"
    );
    let below = game::WorldBlockPos::new(at.x, at.y - 1, at.z);
    assert!(seen_from(&mut w.pair, hb, b, below).is_some(), "the ground under the dig is still there");
    w.until_committed_after(&db, 0);
    assert_eq!(prepared(&db), 0, "nothing stays prepared");
    w.stop(&dir);
}

// ---- R4 Task 5: claiming on demand --------------------------------------------------------


/// Two hosts of a fresh world under `claim`: nothing is claimed until something needs a
/// region. A client (an observer session on host 0) asks for one region in each of 8
/// super-regions — asking again each round, as a client re-asks — until the store holds
/// an owner for each: every owner is the one `static` gives (`rendezvous_owner` with every
/// host live, R4 decision 11), and both hosts' directories say so.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn fresh_world_under_claim_lays_out_like_static() {
    install_props();
    let Some(db) = PgDb::create("claimlayout") else { return };
    let dir = scratch("claimlayout");
    let mut sims = Vec::new();
    let mut links = Vec::new();
    let mut claims = Vec::new();
    for ordinal in (0..u64::from(HOSTS)).map(HostId) {
        let (sim, l, c) = claim_host(&db, &dir, 2000 + ordinal.0);
        sims.push(sim);
        links.push(l);
        claims.push(c);
    }
    let s = claims[0].store().super_region_regions();
    let mut pair = Pair { sims, started: Instant::now(), tick: Arc::new(std::sync::atomic::AtomicU64::new(0)) };
    pair.until("every peer synced", |p| p.sims.iter().all(|s| s.manager.peers_synced()));
    assert!(claims[0].store().owners().unwrap().is_empty(), "a fresh world: nothing claimed at boot");
    let (visitor, player) = (5, PlayerId(77));
    let now = pair.now();
    assert!(pair.sims[0].manager.handle_server_event(ServerEvent::ObserverConnected { client: visitor, player }, now));
    let visits = super_region_corners(s, 8);
    let store = claims[0].store().clone();
    pair.until("every visited super-region claimed", |p| {
        let now = p.now();
        for (_, rc) in &visits {
            let ev = ServerEvent::ClientPacket(game::ClientPacket::RequestRegionConnection(*rc), visitor);
            assert!(p.sims[0].manager.handle_server_event(ev, now));
        }
        let owners = store.owners().unwrap();
        visits.iter().all(|(sr, _)| owners.iter().any(|(o, owner, _)| o == sr && *owner != game::NO_OWNER))
    });
    let owners = store.owners().unwrap();
    for (sr, rc) in &visits {
        let row = owners.iter().find(|(o, _, _)| o == sr).expect("a row");
        let preferred = game::rendezvous_among(*sr, (0..u64::from(HOSTS)).map(HostId)).expect("an owner");
        assert_eq!(row.1, (preferred.0 as i64), "{sr:?} is claimed as a fixed table of the live ids lays it out");
        pair.until("both directories know it", |p| p.sims.iter().all(|sim| sim.manager.owner_host(*rc) == Some(preferred)));
    }
    // Each host's fence holds exactly what it claimed.
    for (h, c) in claims.iter().enumerate() {
        let mine: Vec<game::SuperRegion> =
            owners.iter().filter(|(_, o, _)| *o == h as i64).map(|(sr, _, _)| *sr).collect();
        assert_eq!(c.fence().epochs.keys().copied().collect::<Vec<_>>(), mine, "host {h}'s fence");
    }
    drop(pair);
    drop(links);
    for c in claims {
        c.store().clone().shutdown();
    }
    assert_eq!(prepared(&db), 0, "nothing stays prepared");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Elastic 1 (was: a host restarted cleanly re-claims its own rows): a host under `claim`
/// that left cleanly (its `hosts` row clean, as a committed leave cut leaves it) and starts
/// again is a NEW host: a new id at once, owning nothing — the land stays the clean row's,
/// at the same epoch (a `Leave` succession gives it away when a host needs it) — and its
/// directory says so.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn a_host_restarted_cleanly_is_a_new_host_owning_nothing() {
    install_props();
    let Some(db) = PgDb::create("claimrestart") else { return };
    let dir = scratch("claimrestart");
    let (first, first_links, first_claims) = claim_host(&db, &dir, 3001);
    let old = first.ordinal;
    let store = first_claims.store().clone();
    let mine = vec![(0, 1), (2, 2)];
    first_claims.add(store.claim_free(old, 3001, mine.clone()).unwrap());
    let before = store.owners().unwrap();
    drop(first);
    drop(first_links);
    // Its leave cut committed (as `two_phase` marks it).
    exec(&db, &format!("UPDATE hosts SET clean = 1 WHERE id = {}", old.0));
    let (second, second_links, second_claims) = claim_host(&db, &dir, 3002);
    assert!(second.ordinal > old, "a new id: {} after {old}", second.ordinal);
    assert!(second_claims.fence().epochs.is_empty(), "it owns nothing");
    let after = store.owners().unwrap();
    for sr in &mine {
        let was = before.iter().find(|(o, _, _)| o == sr).expect("claimed before");
        let now = after.iter().find(|(o, _, _)| o == sr).expect("still a row");
        assert_eq!(now, was, "{sr:?} is still the clean row's, at the same epoch");
        let rc = RegionCoords::new(sr.0 * store.super_region_regions() as i32, sr.1 * store.super_region_regions() as i32);
        assert_eq!(second.manager.owner_of(rc), game::Owner::Host(old), "{sr:?}: the new host's directory names the old id");
    }
    drop(second);
    drop(second_links);
    first_claims.store().clone().shutdown();
    second_claims.store().clone().shutdown();
    assert_eq!(prepared(&db), 0, "nothing stays prepared");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Elastic 1 Task 6: the login service links to the coordinator, which relays to a live host.
mod login_link {
    use super::*;

    /// A real coordinator and real hosts over real links on PostgreSQL: a `SpawnPlayer` on the
    /// coordinator's login listener comes back as `SpawnAck { ok: true, quic: Some(_) }` (the
    /// gate host, host 0, took it: the player's record is in its land), and a `Kick` as
    /// `KickAck`.
    #[test]
    #[ignore = "slow: >100ms — run via just test-full"]
    fn the_coordinator_relays_a_spawn_and_its_ack() {
        install_props();
        let Some(db) = PgDb::create("lrelay") else { return };
        let dir = scratch("lrelay");
        let mut w = login_hosts(&db, &dir);
        let (a0, _) = sides(w.w.seam, h(0), &w);
        let token = w.player(7, PlayerId(910), Some(a0));
        let ack = w.join(&token, 1);
        assert!(matches!(&ack, HostToLogin::SpawnAck { ok: true, quic: Some(_), .. }), "{ack:?}");
        assert_eq!(ack_quic(&ack), (true, endpoint(h(0)).quic), "the gate host took it");
        let from = w.login.seen.len();
        w.login.tx.send(LoginToHost::Kick { player_token: token.clone(), session_epoch: 1, reason: "signed out".into() }).unwrap();
        let kicked = HostToLogin::KickAck { player_token: token.clone(), session_epoch: 1 };
        w.until_login("the kick ack", from, |m| *m == kicked);
        w.rounds(20);
        assert_eq!(w.login.seen[from..].iter().filter(|m| **m == kicked).count(), 1, "acknowledged once");
        w.stop(&dir);
    }

    /// Ruling 3: a session ends while the login service is away from the coordinator; its
    /// `SessionEnded` waits at the coordinator and reaches the login service once it is back,
    /// once.
    #[test]
    #[ignore = "slow: >100ms — run via just test-full"]
    fn a_session_end_while_the_login_service_is_away_reaches_it_on_its_return() {
        install_props();
        let Some(db) = PgDb::create("lheld") else { return };
        let dir = scratch("lheld");
        let mut w = login_hosts(&db, &dir);
        let (b1, _) = sides(w.w.seam, h(1), &w);
        let token = w.player(8, PlayerId(911), Some(b1));
        assert_eq!(ack_quic(&w.join(&token, 1)), (true, endpoint(h(1)).quic));
        let client = w.connect(h(1), &token, 1);
        let deadline = Instant::now() + WAIT;
        while w.bodies(client).is_empty() {
            assert!(Instant::now() < deadline, "the player never got a body");
            w.rounds(1);
        }
        // The login service goes away (its connection closes) and the coordinator notices.
        let away = std::mem::replace(&mut w.login, Login { tx: unbounded().0, events: unbounded().1, seen: Vec::new() });
        drop(away);
        let settle = Instant::now() + Duration::from_secs(1);
        while Instant::now() < settle {
            w.w.pair.round();
        }
        w.disconnect(h(1), client, &token, 1);
        for _ in 0..40 {
            w.w.pair.round();
        }
        // Back: the held end arrives, once.
        w.login = dial_login(&w.login_at);
        let want = HostToLogin::SessionEnded { player_token: token.clone(), session_epoch: 1 };
        w.until_login("the held session end", 0, |m| *m == want);
        w.rounds(20);
        assert_eq!(w.ended(&token, 1), 1, "once");
        w.stop(&dir);
    }
}

// ---- Elastic 1 Task 8: the host drains with a warning ----------------------------------

/// `GET path` on the health endpoint at `addr`: (status, body).
fn http_get(addr: std::net::SocketAddr, path: &str) -> (u16, String) {
    use std::io::{Read, Write};
    let mut stream = std::net::TcpStream::connect(addr).expect("connect");
    stream.set_read_timeout(Some(Duration::from_secs(5))).expect("timeout");
    write!(stream, "GET {path} HTTP/1.1\r\nHost: test\r\nConnection: close\r\n\r\n").expect("request");
    let mut response = String::new();
    stream.read_to_string(&mut response).expect("response");
    let status = response.split(' ').nth(1).and_then(|s| s.parse().ok()).expect("a status line");
    let body = response.split_once("\r\n\r\n").map(|(_, b)| b.to_string()).unwrap_or_default();
    (status, body)
}

/// Host `h` beats its lease from its main loop (as `server::run`'s does), and with it reads
/// its drain mark.
fn beat(w: &mut World, h: HostId) {
    let store = w.claims[h.0 as usize].store().clone();
    let boot = sim(w, h).boot;
    sim(w, h).heartbeat = Some(server::lease::Heartbeat::new(store, h, boot));
}

/// `worldctl drain <url> 1 --in 1` against two real hosts and a real coordinator: host 1 hears
/// the mark at its next beat, its deadline comes, it leaves through a committed leave cut (its
/// row clean) — and then does NOT exit: it stays up drained. `/readyz` answers 503 `drained`,
/// `/livez` 200, a connecting client is refused with the leaving refusal; 3 s later it still
/// idles, also after its `hosts` row was deleted (a registration's prune may do that), and no
/// new `hosts` row appeared (nothing restarted it as a new host). A stop signal: exit 0.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn a_drained_host_stays_up_unready_and_exits_zero_on_sigterm() {
    install_props();
    let Some(db) = PgDb::create("ddrained") else { return };
    let dir = scratch("ddrained");
    let mut w = world(&db, &dir);
    w.until_committed_after(&db, 0);
    beat(&mut w, h(1));
    let rows_before = query_i64s(&db, "SELECT COUNT(*) FROM hosts");
    w.claims[0].store().drain_host(h(1), 1_000).expect("worldctl drain 1 --in 1");
    w.pair.until("host 1 heard its drain", |p| p.sims.iter().any(|s| s.ordinal == h(1) && s.manager.draining().is_some()));
    w.pair.until("its deadline came", |p| p.sims.iter().any(|s| s.ordinal == h(1) && s.manager.drain_due()));
    let (exit, leave) = exit_host_leaving(&mut w, h(1), None, |_, _| {});
    assert_eq!((exit, leave), (server::claims::ServerExit::Clean, Some(true)), "left in a committed leave cut");
    assert!(server::drain::stays_drained(true, exit, leave), "it stays up drained");
    assert_eq!(query_i64s(&db, "SELECT clean FROM hosts WHERE id = 1"), [1], "its row is clean");
    // The drained process: its health endpoint, its edge's events, its stop signal.
    let health = server::health::Health::for_test();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    server::health::serve_for_test(listener, "host", health).unwrap();
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (ev_tx, ev_rx) = unbounded();
    let (out_tx, out_rx) = unbounded();
    let flag = stop.clone();
    let idle = std::thread::spawn(move || server::drain::idle_drained(&flag, &ev_rx, &out_tx, health));
    let ready = || http_get(addr, "/readyz");
    let deadline = Instant::now() + WAIT;
    while ready().0 != 503 {
        assert!(Instant::now() < deadline, "never unready");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(ready(), (503, "drained".to_string()));
    assert_eq!(http_get(addr, "/livez").0, 200);
    ev_tx.send(ServerEvent::connected(77)).unwrap();
    let (to, refusal) = out_rx.recv_timeout(Duration::from_secs(5)).expect("the connecting client is answered");
    assert_eq!(to, Some(77));
    assert!(matches!(&refusal, ServerPacket::Refused(why) if why == server::claims::LEAVING_REFUSAL), "{refusal:?}");
    // Host 0 runs on meanwhile; host 1's process idles.
    let alive_until = Instant::now() + Duration::from_secs(3);
    while Instant::now() < alive_until {
        w.pair.round();
    }
    assert!(!idle.is_finished(), "alive 3 s later");
    exec(&db, "DELETE FROM hosts WHERE id = 1");
    for _ in 0..20 {
        w.pair.round();
    }
    assert!(!idle.is_finished(), "its row pruned: still up");
    assert_eq!(http_get(addr, "/livez").0, 200, "alive");
    assert_eq!(ready(), (503, "drained".to_string()));
    assert_eq!(query_i64s(&db, "SELECT COUNT(*) FROM hosts")[0], rows_before[0] - 1, "no new hosts row");
    stop.store(true, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(idle.join().unwrap(), server::claims::ServerExit::Clean, "SIGTERM: exit 0");
    w.stop(&dir);
}

/// A stop signal during the warning means "leave now": host 1 is told a drain 300 s away; the
/// stop comes — it leaves at once through a committed leave cut and exits (it does not wait for
/// the deadline, and it does not stay up drained: that is only for the deadline's leave).
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn sigterm_during_the_warning_leaves_at_once() {
    install_props();
    let Some(db) = PgDb::create("dsigterm") else { return };
    let dir = scratch("dsigterm");
    let mut w = world(&db, &dir);
    w.until_committed_after(&db, 0);
    beat(&mut w, h(1));
    w.claims[0].store().drain_host(h(1), 300_000).expect("worldctl drain 1 --in 300");
    w.pair.until("host 1 heard its drain", |p| p.sims.iter().any(|s| s.ordinal == h(1) && s.manager.draining().is_some()));
    assert!(!sim(&mut w, h(1)).manager.drain_due(), "not due for 300 s");
    let stopped = Instant::now();
    let (exit, leave) = exit_host_leaving(&mut w, h(1), None, |_, _| {});
    assert_eq!((exit, leave), (server::claims::ServerExit::Clean, Some(true)), "left in a committed leave cut");
    assert!(stopped.elapsed() < Duration::from_secs(30), "at once, not at the deadline: {:?}", stopped.elapsed());
    assert!(!server::drain::stays_drained(false, exit, leave), "a stop signal exits");
    assert_eq!(query_i64s(&db, "SELECT clean FROM hosts WHERE id = 1"), [1], "its row is clean");
    w.stop(&dir);
}
