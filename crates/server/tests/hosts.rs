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
        fortress: None,
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

/// A host dialing with another service token never comes up on either side.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn peer_with_a_wrong_token_is_refused() {
    let Some(db) = PgDb::create("token") else { return };
    let dir = scratch("token");
    let (_s0, c0) = claim(&db, h(0));
    let (_s1, c1) = claim(&db, h(1));
    let mut l0 = start_links(&c0, h(0), 1, TOKEN, &dir, "127.0.0.1:0");
    let mut l1 = start_links(&c1, h(1), 2, "not-the-token", &dir, "127.0.0.1:0");
    let (o0, mut to1) = only_link(&mut l0);
    let (o1, mut to0) = only_link(&mut l1);
    assert_eq!((o0, o1), (h(1), h(0)));
    // Long enough for the dialer to find the row and try a few times.
    let deadline = Instant::now() + Duration::from_secs(4);
    while Instant::now() < deadline {
        to1.pump(0);
        to0.pump(0);
        assert!(!to1.is_up() && !to0.is_up(), "a peer with a wrong token came up");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!((to1.connection(), to0.connection()), (0, 0));
    drop((to1, to0, l0, l1));
    c0.store().clone().shutdown();
    c1.store().clone().shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

/// The lower host restarts on another port (same pin file, a new boot, a new row): the
/// higher one's dialer picks the new row up and is connected again within 5 s, as a new
/// connection, and frames flow.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn restarted_listener_is_found_again() {
    let Some(db) = PgDb::create("restart") else { return };
    let dir = scratch("restart");
    let (_s0, c0) = claim(&db, h(0));
    let (_s1, c1) = claim(&db, h(1));
    let mut l0 = start_links(&c0, h(0), 1, TOKEN, &dir, "127.0.0.1:0");
    let mut l1 = start_links(&c1, h(1), 2, TOKEN, &dir, "127.0.0.1:0");
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
    c0.store().clone().shutdown();
    pump_until("host 1 sees the link down", WAIT, &mut [&mut to0], |l| !l[0].is_up());

    // Host 0 starts again: a new claim, a new boot, another port.
    let new_bind = loop {
        let addr = std::net::UdpSocket::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
        if addr != old_addr {
            break addr;
        }
    };
    let (_s0, c0) = claim(&db, h(0));
    let mut l0 = start_links(&c0, h(0), 3, TOKEN, &dir, &new_bind.to_string());
    assert_eq!(l0.local_addr(), new_bind);
    let (_, mut to1) = only_link(&mut l0);
    pump_until("found again within 5 s", Duration::from_secs(5), &mut [&mut to1, &mut to0], |l| l.iter().all(|l| l.is_up()));
    assert_eq!(to0.connection(), 2, "a new connection");
    to0.send(PeerMsg::Ack { through: 7 });
    pump_until("a frame over the new connection", WAIT, &mut [&mut to1], |l| l[0].recv() == Some(PeerMsg::Ack { through: 7 }));
    drop((to1, to0, l0, l1));
    c0.store().clone().shutdown();
    c1.store().clone().shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

/// Several hosts start only as `host`, on Postgres, with the login link (`jwt` with a
/// service token); one host (or `all`) as before.
#[test]
fn several_hosts_refuse_local_auth() {
    assert_eq!(check_several_hosts(Role::Host, 2, true, false), Err(SEVERAL_HOSTS_NEED_LOGIN.to_string()));
    assert_eq!(SEVERAL_HOSTS_NEED_LOGIN, "several hosts need the login link: ids and placement come from it");
    assert_eq!(check_several_hosts(Role::Host, 2, true, true), Ok(()));
    assert_eq!(check_several_hosts(Role::All, 2, true, true), Err(SEVERAL_HOSTS_NEED_ROLE_HOST.to_string()));
    assert_eq!(check_several_hosts(Role::Host, 3, false, true), Err(SPLIT_ROLES_NEED_POSTGRES.to_string()));
    for role in [Role::All, Role::Host] {
        assert_eq!(check_several_hosts(role, 1, false, false), Ok(()), "{role:?}: one host as before");
    }
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
/// `CutPrepared` (before the decision): the cut commits whole; host 1 reboots dirty (the
/// generation + 1), host 0 rolls back, and both restart from that cut.
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
    let again = restart_host(&mut w, &db, &dir, h(1), 3001);
    assert_eq!((again.generation, again.bumped), (generation + 1, true), "host 1 came back dirty");
    roll_back_and_restart_the_other(&mut w, &db, &dir, h(1), generation + 1);
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
/// aborted (host 1's detach), nothing stays prepared; host 1 reboots dirty and every host
/// rolls back to the previous cut.
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
    let again = restart_host(&mut w, &db, &dir, h(1), 3001);
    assert_eq!((again.generation, again.bumped), (generation + 1, true), "host 1 came back dirty");
    roll_back_and_restart_the_other(&mut w, &db, &dir, h(1), generation + 1);
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
    server::roles::recover_staged_cuts(&store, HOSTS).expect("the staged parts are resolved");
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
    server::roles::recover_staged_cuts(&store, HOSTS).expect("the staged parts are resolved");
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
/// the last cut, and restarts: the generation is raised exactly once, host 0 exits
/// `WorldRolledBack` once and restarts, both stay up, and the slime is in the world
/// exactly once (where the last cut has it).
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
    let again = restart_host(&mut w, &db, &dir, h(1), 3001);
    assert_eq!((again.generation, again.bumped), (generation + 1, true), "host 1 came back dirty: + 1");
    roll_back_and_restart_the_other(&mut w, &db, &dir, h(1), generation + 1);
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
/// left prepared. The other host's cuts carry on without it: a dead row is not expected.
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
    // Host 0's cuts go on: host 1's dead row is no longer expected in a cut.
    let (last, _) = last_committed(&db);
    w.pair.until("a world cut of host 0 alone committed", |_| decisions(&db).iter().any(|(c, _, k)| *k && *c > last));
    w.pair.until("host 0 settled it", |p| p.sims.iter().all(|s| s.cuts.as_ref().unwrap().world_part().is_none()));
    assert_eq!(saved(&db), before, "nothing of host 1's region saved after it was replaced");
    // Conservation, as far as this hand-made death allows. The row was marked dead by
    // hand, under `Rollback`, with no succession: nothing adopts host 1's last committed
    // part. (In the product only a declaration marks a row dead, in the same transaction
    // as the succession that hands the land to an heir, and no cut commits until one
    // settles it — the heir's part adopting those rows, the survivors' journals saying
    // what crossed since.) So an entity that crossed from host 1 to host 0 after host 1's
    // last committed cut is in host 0's rows now AND still in host 1's stale ones — that
    // and nothing else: every entity stored twice is exactly one row of host 0's and one
    // of a host 1 region never saved again (at or before host 1's last saved tick).
    let s = w.claims[0].store().super_region_regions();
    let owner = |rx: i64, rz: i64| {
        let rc = RegionCoords::new(rx as i32, rz as i32);
        (0..u64::from(HOSTS)).map(HostId).find(|&h| game::Static { id: h, hosts: HOSTS }.owns(rc, s)).expect("an owned region")
    };
    let host1_last = before.iter().copied().max().expect("host 1's region was saved");
    let twice = entity_rows_stored_twice(&db);
    for rows in twice.chunk_by(|a, b| a[..3] == b[..3]) {
        let mut owners: Vec<HostId> = rows.iter().map(|r| owner(r[3], r[4])).collect();
        owners.sort();
        assert_eq!(owners, [h(0), h(1)], "an entity stored twice is one row of host 0's and one of host 1's: {twice:?}");
        let stale = rows.iter().find(|r| owner(r[3], r[4]) == h(1)).unwrap();
        assert!(stale[5] <= host1_last, "host 1's copy is from its last committed part (tick {host1_last}): {twice:?}");
        let fresh = rows.iter().find(|r| owner(r[3], r[4]) == h(0)).unwrap();
        assert!(fresh[5] > host1_last, "host 0's copy is from a cut it took alone: {twice:?}");
    }
    assert_eq!(prepared(&db), 0, "nothing stays prepared");
    w.stop(&dir);
}

/// Host 1 leaves cleanly (`exit_world` with several hosts: a leave cut): it exits 0 with
/// its row clean. A slime sent toward it meanwhile waits in host 0's session and is host
/// 0's row in its next cut. Host 1 returns: the generation is unchanged, and the slime
/// arrives there once.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn clean_leave_and_return_keeps_the_generation_and_delivers_waiting_transfers() {
    install_props();
    let Some(db) = PgDb::create("leave") else { return };
    let dir = scratch("leave");
    let mut w = world(&db, &dir);
    let generation = w.claims[0].store().world_generation().unwrap();
    w.until_committed_after(&db, 0);
    assert_eq!(exit_host(&mut w, h(1)), server::claims::ServerExit::Clean, "host 1 left");
    let clean: Vec<i64> = query_i64s(&db, "SELECT clean FROM hosts WHERE id = 1");
    assert_eq!(clean, [1], "host 1's row is clean");
    let (left_in, _) = last_committed(&db);
    assert_eq!(prepared(&db), 0, "nothing stays prepared");
    // A slime toward host 1's region: it waits on host 0, a row of host 0's next cut.
    let (a0, b1) = (seam_of(&w, h(0)), seam_of(&w, h(1)));
    let past = if b1.x != a0.x {
        if b1.x > a0.x { REGION_SIZE + game::FLIP_HYSTERESIS + 2.0 } else { -game::FLIP_HYSTERESIS - 2.0 }
    } else {
        REGION_SIZE / 2.0
    };
    let id = spawn_slime_in(&mut w, h(0), a0, REGION_SIZE / 2.0);
    sim(&mut w, h(0)).manager.spawner_mut().with_region(a0, |region| {
        let key = region.data().id_index[&id];
        let y = above_ground_y();
        let at = if b1.x != a0.x {
            game::rapier3d::math::Vector::new(past, y, REGION_SIZE / 2.0)
        } else {
            let z = if b1.z > a0.z { REGION_SIZE + game::FLIP_HYSTERESIS + 2.0 } else { -game::FLIP_HYSTERESIS - 2.0 };
            game::rapier3d::math::Vector::new(REGION_SIZE / 2.0, y, z)
        };
        region.with_data(|d| d.set_body_pose_safe(key, game::Pose::from_translation(at)));
    });
    w.pair.until("the slime waits in host 0's session", |p| p.sims[0].manager.peer_unreceived_ids().iter().any(|(_, i, _)| *i == id));
    w.until_committed_after(&db, left_in);
    let rows = query_i64s(
        &db,
        &format!("SELECT COUNT(*) FROM in_flight WHERE host = 0 AND kind = 0 AND target_rx = {} AND target_rz = {}", b1.x, b1.z),
    );
    assert_eq!(rows, [1], "host 0's cut holds the waiting slime as a row");
    // Host 1 returns.
    let again = restart_host(&mut w, &db, &dir, h(1), 3001);
    assert_eq!((again.generation, again.bumped), (generation, false), "a clean return keeps the generation");
    w.pair.until("the slime arrived on host 1", |p| {
        p.sims.iter().all(|s| s.manager.peer_unreceived() == 0) && p.sims[1].holders(id) == [b1]
    });
    for _ in 0..40 {
        w.pair.round();
    }
    // It arrived on host 1 (above); slimes wander, so only once is asserted now.
    assert_eq!(holders_all(&mut w, id).len(), 1, "once: {:?}", holders_all(&mut w, id));
    for h in (0..u64::from(HOSTS)).map(HostId) {
        assert!(!w.generations[h.0 as usize].is_set(), "no rollback");
    }
    w.until_committed_after(&db, last_committed(&db).0);
    assert_eq!(prepared(&db), 0, "nothing stays prepared");
    w.stop(&dir);
}

/// Final review I1: host 1 returns from a clean leave while a cut of host 0 alone is
/// pending (begun, host 0's part not staged yet): that cut is aborted — by the coordinator
/// on host 1's attach, or by the store's decision (host 1's row is dirty and unlisted) —
/// the next cut takes both and commits, and a slime of host 0's is held and stored once.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn host_returning_beside_a_cut_that_excludes_it_aborts_that_cut() {
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
    // A cut of host 0 alone begins; nothing of host 0 runs from here until host 1 is back.
    let cut = until_part(&mut w, 0, &["pending", "marked"]);
    assert_eq!(decision_of(&db, cut), None);
    let again = restart_host(&mut w, &db, &dir, h(1), 3001);
    assert_eq!((again.generation, again.bumped), (generation, false), "a clean return");
    w.pair.until("the cut that excludes host 1 is decided", |_| decision_of(&db, cut).is_some());
    assert_eq!(decision_of(&db, cut), Some(false), "the cut of host 0 alone is aborted");
    w.until_committed_after(&db, cut.0 as i64);
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
    server::roles::resolve_before_claim(true, &all, 1).expect("the start-up resolves staged parts");
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
    let g1 = server::claims::WorldGeneration::new(links[1].registered().generation);
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
    // Its return raises the generation.
    let (_, c1) = claim(&db, h(1));
    let back = start_links(&c1, h(1), 4001, TOKEN, &dir, "127.0.0.1:0");
    assert_eq!((back.registered().generation, back.registered().bumped), (generation + 1, true));
    drop(back);
    drop(pair);
    drop(links);
    c1.store().clone().shutdown();
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

/// The bound on both hosts' clean leave when they are stopped together: well inside the
/// supervisor's 95 s deadline (the leave batch, a cut's lead and its decision take ~3 s).
const BOTH_LEAVE_WITHIN: Duration = Duration::from_secs(15);

/// Slime `id` in host `h`'s seam region, put just past the seam toward the other host's.
fn push_over_the_seam(w: &mut World, h: HostId, id: EntityId) {
    let (a, _) = w.seam;
    let from = seam_of(w, h);
    // The seam is `b = a + x`: from `a` over +x, from `b` over -x.
    let x = if from == a { REGION_SIZE + game::FLIP_HYSTERESIS + 2.0 } else { -game::FLIP_HYSTERESIS - 2.0 };
    sim(w, h).manager.spawner_mut().with_region(from, |region| {
        let key = region.data().id_index[&id];
        let at = game::rapier3d::math::Vector::new(x, above_ground_y(), REGION_SIZE / 2.0);
        region.with_data(|d| d.set_body_pose_safe(key, game::Pose::from_translation(at)));
    });
}

/// What one host's `exit_world` came to: its outcome, how long it took from the shared
/// stop signal, and the leave cut that named it (`leaving_cut`).
struct Exited {
    exit: server::claims::ServerExit,
    took: Duration,
    leaving_cut: Option<game::CutId>,
}

/// Every host gets the stop signal at the same moment (as the dev stack's supervisor
/// TERMs them all): each runs the real `exit_world` on its own thread, its regions ticking
/// at its own pace, the coordinator reading the highest world tick. Their outcomes, by
/// ordinal; their peer listeners and coordinator links go with them.
fn exit_every_host_at_once(w: &mut World) -> Vec<Exited> {
    let mut sims: Vec<Sim> = std::mem::take(&mut w.pair.sims);
    sims.sort_by_key(|s| s.ordinal);
    let started = w.pair.started;
    let tick = w.pair.tick.clone();
    let stop = std::sync::Barrier::new(sims.len());
    let params: Vec<(server::claims::Fenced, server::claims::WorldGeneration)> =
        sims.iter().map(|s| (w.claims[s.ordinal.0 as usize].fenced().clone(), w.generations[s.ordinal.0 as usize].clone())).collect();
    let exited = std::thread::scope(|scope| {
        let handles: Vec<_> = sims
            .iter_mut()
            .zip(&params)
            .map(|(sim, (fenced, generation))| {
                let (tick, stop) = (tick.clone(), &stop);
                scope.spawn(move || {
                    let region_out = sim.region_out.clone();
                    let cuts = sim.cuts.take();
                    stop.wait();
                    let at = Instant::now();
                    let exit = server::cuts::exit_world(
                        &mut sim.manager,
                        server::cuts::ExitParams {
                            region_out: &region_out,
                            now_ms: || started.elapsed().as_millis() as u64,
                            cuts,
                            coordinator: None,
                            fenced,
                            world_generation: generation,
                            park_timeout: server::cuts::EXIT_PARK_TIMEOUT,
                            save_timeout: Duration::from_secs(10),
                            now_tick: || 0,
                        },
                        |m| {
                            m.spawner_mut().tick_all();
                            m.spawner_mut().pump();
                            tick.fetch_max(m.spawner_mut().world_tick(), std::sync::atomic::Ordering::SeqCst);
                            std::thread::sleep(Duration::from_millis(5));
                        },
                    );
                    Exited { exit, took: at.elapsed(), leaving_cut: sim.manager.leaving_cut() }
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().expect("an exit thread panicked")).collect::<Vec<_>>()
    });
    for sim in sims {
        let h = sim.ordinal;
        drop(sim);
        w.switches[h.0 as usize].drop_link();
        w.links[h.0 as usize] = None;
    }
    exited
}

/// Task 14b: both hosts are stopped at the same moment with the coordinator up (the dev
/// stack's shutdown), slimes crossing the seam both ways as they go. Their leave requests
/// fall in one batch, so the design yields ONE leave cut naming both leaving (there is no
/// stayer): both exit clean well inside the supervisor's deadline, that cut is committed,
/// both `hosts` rows are clean and nothing stays prepared. Restarting both keeps the world
/// generation (no rollback), and each slime is in the world exactly once.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn both_hosts_stopped_together_leave_cleanly() {
    install_props();
    let Some(db) = PgDb::create("bothleave") else { return };
    let dir = scratch("bothleave");
    let mut w = world(&db, &dir);
    let generation = w.claims[0].store().world_generation().unwrap();
    w.until_committed_after(&db, 0);
    let (before, _) = last_committed(&db);
    let slimes: Vec<EntityId> = (0..u64::from(HOSTS))
        .map(HostId)
        .map(|h| {
            let rc = seam_of(&w, h);
            spawn_slime_in(&mut w, h, rc, REGION_SIZE / 2.0)
        })
        .collect();
    w.pair.round();
    for h in (0..u64::from(HOSTS)).map(HostId) {
        push_over_the_seam(&mut w, h, slimes[h.0 as usize]);
    }
    let exited = exit_every_host_at_once(&mut w);
    for (h, e) in exited.iter().enumerate() {
        assert_eq!(e.exit, server::claims::ServerExit::Clean, "host {h} exits 0");
        assert!(e.took < BOTH_LEAVE_WITHIN, "host {h} left in {:?}", e.took);
    }
    let cut = exited[0].leaving_cut.expect("a leave cut named host 0");
    assert_eq!(exited[1].leaving_cut, Some(cut), "one leave cut named both hosts leaving");
    assert_eq!(decision_of(&db, cut), Some(true), "it committed");
    // A periodic cut running at the stop signal may commit before it; none after it, and
    // no leave cut was aborted (asked for again).
    let since: Vec<(i64, i64, bool)> = decisions(&db).into_iter().filter(|(c, _, _)| *c > before).collect();
    assert!(since.iter().all(|(_, _, k)| *k), "no cut aborted since the stop: {since:?}");
    assert_eq!(since.last().map(|(c, _, _)| *c as u64), Some(cut.0), "the leave cut is the last decided: {since:?}");
    assert_eq!(query_i64s(&db, "SELECT clean FROM hosts ORDER BY id"), [1, 1], "both rows are clean");
    assert_eq!(prepared(&db), 0, "nothing stays prepared");
    // Both return: no generation bump, no rollback, each slime once.
    for h in (0..u64::from(HOSTS)).map(HostId) {
        let again = restart_host(&mut w, &db, &dir, h, 3000 + h.0);
        assert_eq!((again.generation, again.bumped), (generation, false), "host {h}'s clean return keeps the generation");
    }
    w.pair.until("both hosts attached again", |p| p.sims.len() == 2 && p.sims.iter().all(|s| s.manager.world_attached()));
    w.pair.until("every slime back in a running region, nothing owed", |p| {
        p.sims.iter().all(|s| s.manager.peer_unreceived() == 0)
            && slimes.iter().all(|id| p.sims.iter_mut().map(|s| s.holders(*id).len()).sum::<usize>() > 0)
    });
    for _ in 0..40 {
        w.pair.round();
    }
    for id in &slimes {
        assert_eq!(holders_all(&mut w, *id).len(), 1, "{id:?} once: {:?}", holders_all(&mut w, *id));
    }
    for h in (0..u64::from(HOSTS)).map(HostId) {
        assert!(!w.generations[h.0 as usize].is_set(), "host {h}: no rollback");
        assert_eq!(sim(&mut w, h).manager.take_higher_generation(), None, "host {h} sees no newer generation");
    }
    assert_eq!(w.claims[0].store().world_generation().unwrap(), generation, "the generation is unchanged");
    w.until_committed_after(&db, cut.0 as i64);
    assert_eq!(prepared(&db), 0, "nothing stays prepared");
    w.stop(&dir);
}

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
    push_over_the_seam(&mut w, h(0), id);
    w.pair.until("the slime on host 1", |p| p.sims[1].holders(id) == [b1]);
    w.until_committed_after(&db, stored);
    let rows = |column: &str| {
        format!(
            "SELECT {column} FROM entities WHERE birth_rx = {} AND birth_rz = {} AND serial = {}",
            id.birth_region.x, id.birth_region.z, id.serial as i64
        )
    };
    let stored_in: Vec<(i64, i64)> = query_i64s(&db, &rows("rx")).into_iter().zip(query_i64s(&db, &rows("rz"))).collect();
    assert_eq!(stored_in, [(i64::from(b1.x), i64::from(b1.z))], "the slime is stored once, in host 1's region");
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

/// Two hosts ([`two_hosts`]) whose login relays run (host 0 the entry host, its login link
/// listening, the login service dialed in), over the players of `roster`.
struct LoginWorld {
    pair: Pair,
    seam: (RegionCoords, RegionCoords),
    links: Vec<PeerLinks>,
    claims: Vec<HostClaims>,
    link: Option<LoginLink>,
    login: Login,
    roster: Roster,
}

/// The entry host of the login tests.
const ENTRY: HostId = HostId(0);

fn login_hosts(db: &PgDb, dir: &std::path::Path) -> LoginWorld {
    let (mut pair, seam, links, claims) = two_hosts(db, dir);
    let roster: Roster = Arc::default();
    let players = server::player_store::shared(RosterPlayers(roster.clone()));
    let mut link = None;
    for sim in &mut pair.sims {
        let (events_tx, events) = unbounded();
        let orders = Arc::new(SpawnOrders::new(server::auth_edge::system_clock()));
        let relay = LoginRelay::new(orders.clone(), players.clone(), events_tx.clone());
        sim.manager.set_entry_host(ENTRY);
        if sim.ordinal == ENTRY {
            let pin = dir.join("host-link.cert-sha256");
            link = Some(
                server::login_link::start_relaying(
                    "127.0.0.1:0".parse().unwrap(),
                    TOKEN.into(),
                    Some(pin),
                    orders.clone(),
                    players.clone(),
                    events_tx,
                    ENTRY,
                )
                .expect("the login link listens"),
            );
        }
        sim.login = Some(SimLogin { orders, relay, events });
    }
    let l: &LoginLink = link.as_ref().unwrap();
    let hello = svc_link::LinkHello::new(TOKEN.into(), svc_link::PeerRole::Login);
    let (tx, events) = svc_link::LinkDialer::connect::<HostToLogin, LoginToHost>(l.local_addr(), l.cert_sha256().to_string(), hello);
    match events.recv_timeout(WAIT) {
        Ok(svc_link::LinkEvent::Up(_)) => {}
        other => panic!("the login link did not come up: {other:?}"),
    }
    let login = Login { tx, events, seen: Vec::new() };
    // Each host's first sync of the other is done (its `Running` came).
    pair.until("every peer synced", |p| p.sims.iter().all(|s| s.manager.peers_synced()));
    LoginWorld { pair, seam, links, claims, link, login, roster }
}

impl LoginWorld {
    fn stop(mut self, dir: &std::path::Path) {
        drop(self.link.take());
        drop(self.pair);
        drop(self.links);
        for c in self.claims {
            c.store().clone().shutdown();
        }
        let _ = std::fs::remove_dir_all(dir);
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
            fortress: None,
            adventure: None,
        });
        self.roster.lock().unwrap().insert(token.clone(), (player, record));
        token
    }

    fn sim(&mut self, h: HostId) -> &mut Sim {
        self.pair.sims.iter_mut().find(|s| s.ordinal == h).expect("the host is up")
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
            self.pair.round();
        }
    }

    /// `rounds` rounds, the login service listening.
    fn rounds(&mut self, rounds: usize) {
        for _ in 0..rounds {
            self.pair.round();
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
        let now = self.pair.now();
        let manager = &mut self.sim(h).manager;
        let ev = ServerEvent::LoginConnected { client, player, record, player_token: token.into(), session_epoch: epoch };
        orders.announce(token, epoch, || assert!(manager.handle_server_event(ev, now))).expect("announced");
        client
    }

    /// The client's connection to host `h` closes, as its edge reports it.
    fn disconnect(&mut self, h: HostId, client: ClientId, token: &str, epoch: u64) {
        let now = self.pair.now();
        assert!(self.sim(h).manager.handle_server_event(ServerEvent::ClientDisconnected(client), now));
        self.orders(h).closed(token, epoch);
    }

    /// Where `client`'s body is: `(host, region)` of every running region holding it.
    fn bodies(&mut self, client: ClientId) -> Vec<(HostId, RegionCoords)> {
        let mut out = Vec::new();
        for sim in &mut self.pair.sims {
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
    if w.pair.sims[0].manager.owner_host(seam.0) == Some(h) { (seam.0, seam.1) } else { (seam.1, seam.0) }
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
    let (b1, _) = sides(w.seam, h(1), &w);
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
    let (a0, b1) = sides(w.seam, h(0), &w);
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
    while w.bodies(c1) != [(h(1), b1)] || w.pair.sims.iter().any(|s| s.manager.peer_unreceived() > 0) {
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
    let (b1, _) = sides(w.seam, h(1), &w);
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

/// The host holding a player's record region cannot be reached: the entry host refuses the
/// spawn order (the login service answers 503 `world_unavailable`).
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn spawn_for_an_unreachable_owner_is_refused() {
    install_props();
    let Some(db) = PgDb::create("lunreach") else { return };
    let dir = scratch("lunreach");
    let mut w = login_hosts(&db, &dir);
    let (b1, _) = sides(w.seam, h(1), &w);
    let token = w.player(4, PlayerId(903), Some(b1));
    // Host 1 is gone: its manager and its links.
    let i = w.pair.sims.iter().position(|s| s.ordinal == h(1)).unwrap();
    drop(w.pair.sims.remove(i));
    drop(w.links.remove(1));
    w.pair.until("host 0 no longer reaches host 1", |p| !p.sims[0].manager.reaches(b1));
    let ack = w.join(&token, 1);
    assert_eq!(ack_quic(&ack), (false, None), "refused");
    assert!(w.orders(h(0)).pending().by_token.is_empty(), "nothing ordered on host 0");
    // The login service itself, over its own link to host 0: the join answers 503.
    let (status, code) = login_join(&mut w, &token);
    assert_eq!(status, 503);
    assert_eq!(code, login_api::ApiErrorCode::WorldUnavailable);
    assert!(w.orders(h(0)).pending().by_token.is_empty(), "nothing ordered on host 0");
    w.stop(&dir);
}

/// A real login service (its HTTP API in-process, an in-memory store, its own link to the
/// entry host) joins the account that owns `token` to the world while the hosts run round
/// by round: the join's HTTP status and, for a failure, its error code.
fn login_join(w: &mut LoginWorld, token: &str) -> (u16, login_api::ApiErrorCode) {
    use tower::ServiceExt;
    const WORLD: &str = "hosts";
    const NOW: u64 = 1_800_000_000;
    let link = w.link.as_ref().unwrap();
    let (addr, pin) = (link.local_addr().to_string(), link.cert_sha256().to_string());
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
        let err: login_api::ApiError = serde_json::from_slice(&bytes).unwrap_or_else(|e| panic!("{e}: {bytes:?}"));
        (status, err.code)
    });
    let deadline = Instant::now() + WAIT;
    while !joined.is_finished() {
        assert!(Instant::now() < deadline, "the join never answered");
        w.rounds(1);
    }
    let out = rt.block_on(joined).unwrap();
    drop(rt);
    out
}

/// A host that left cleanly (a committed leave cut, its row clean) is what a booting host
/// reads as left (`peer_link::left_cleanly`, seeding `note_left_peers`); once it is back
/// (its new boot registered) it is not, and a crashed host (its row dirty) never is.
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
    restart_host(&mut w, &db, &dir, h(1), 3001);
    assert_eq!(server::peer_link::left_cleanly(&store0, h(0)).unwrap(), [], "back: its new boot cleared the row");
    kill_host(&mut w, h(1));
    assert_eq!(server::peer_link::left_cleanly(&store0, h(0)).unwrap(), [], "crashed: its row stays dirty");
    assert_eq!(prepared(&db), 0, "nothing stays prepared");
    w.stop(&dir);
}

/// A session on host 1 disconnects: its end reaches the login service through host 0.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn session_ended_on_a_non_entry_host_reaches_login() {
    install_props();
    let Some(db) = PgDb::create("lended") else { return };
    let dir = scratch("lended");
    let mut w = login_hosts(&db, &dir);
    let (b1, _) = sides(w.seam, h(1), &w);
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
    let (a0, b1) = sides(w.seam, h(0), &w);
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
    while w.bodies(client) != [(h(1), b1)] || w.pair.sims.iter().any(|s| s.manager.peer_unreceived() > 0) {
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
        let (sim, l, c) = claim_host(&db, &dir, ordinal, 2000 + ordinal.0);
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
        assert_eq!(row.1, (game::rendezvous_owner(*sr, HOSTS).0 as i64), "{sr:?} is claimed as static lays it out");
        pair.until("both directories know it", |p| {
            p.sims.iter().all(|sim| sim.manager.owner_host(*rc) == Some(game::rendezvous_owner(*sr, HOSTS)))
        });
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

/// A host under `claim` that left cleanly (its `hosts` row clean, as a committed leave cut
/// leaves it) boots again: it registers (no wait: its predecessor left clean), re-claims the
/// rows the store says are its own at a higher epoch — the old boot's fence is refused from
/// then on — and its directory answers `Me` for them.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn a_host_restarted_cleanly_reclaims_its_own_rows() {
    install_props();
    let Some(db) = PgDb::create("claimrestart") else { return };
    let dir = scratch("claimrestart");
    let (first, first_links, first_claims) = claim_host(&db, &dir, h(1), 3001);
    let store = first_claims.store().clone();
    let mine = vec![(0, 1), (2, 2)];
    first_claims.add(store.claim_free(h(1), 3001, mine.clone()).unwrap());
    let before = store.owners().unwrap();
    drop(first);
    drop(first_links);
    // Its leave cut committed (as `two_phase` marks it).
    exec(&db, "UPDATE hosts SET clean = 1 WHERE id = 1");
    let (second, second_links, second_claims) = claim_host(&db, &dir, h(1), 3002);
    let after = store.owners().unwrap();
    for sr in &mine {
        let old = before.iter().find(|(o, _, _)| o == sr).expect("claimed before");
        let new = after.iter().find(|(o, _, _)| o == sr).expect("still a row");
        assert_eq!(new.1, 1, "{sr:?} is still host 1's");
        assert!(new.2 > old.2, "{sr:?} re-claimed at a higher epoch: {} then {}", old.2, new.2);
        assert_eq!(second_claims.fence().epochs.get(sr), Some(&new.2), "{sr:?} in the new boot's fence");
        let rc = RegionCoords::new(sr.0 * store.super_region_regions() as i32, sr.1 * store.super_region_regions() as i32);
        assert_eq!(second.manager.owner_of(rc), game::Owner::Me, "{sr:?}: the new boot's directory says Me");
    }
    let cut = game::WorldCut { id: game::CutId(1), host: h(1), tick: 1, regions: vec![], in_flight: vec![], players: vec![], history: None, adopts: vec![] };
    let refused = first_claims.commit_cut(cut).recv().unwrap();
    assert!(matches!(refused, Err(world_store::StoreError::Fenced { .. })), "the old boot is fenced: {refused:?}");
    drop(second);
    drop(second_links);
    first_claims.store().clone().shutdown();
    second_claims.store().clone().shutdown();
    assert_eq!(prepared(&db), 0, "nothing stays prepared");
    let _ = std::fs::remove_dir_all(&dir);
}
