# The adventure selection screen leaves the world — research, parked

Date: 2026-11-19. **Status: PARKED — waiting on the stateless coordinator.** Do not implement this until
the coordinator works like a stateless web service (any replica answers any request from a client or a
host, no connection tied to a replica). That rework is a separate project, owned by another agent. When
it lands, re-read this note, redo "Open, to be redesigned on the stateless coordinator" against it, write
the design for approval, then plan.

## What the owner wants, and why

The owner, after ending an adventure (2026-11-19): *"when the adventure ends, I should not be in game
anymore. There should be nothing behind the adventure selection menu."* And on how: *"make the required
data available such that we can get the world data from, say, the coordinator and then once a selection
is made, connect to the region"*.

Decided in conversation (the owner's words or choices):
- The selection screen's data comes from the **coordinator**; the client connects to a host only once a
  site is picked.
- The client talks to the coordinator over **QUIC / WebTransport** (not through the login service over
  HTTP), with the coordinator's replicas behind **one Service**.
- *"I want the coordinator to work like a stateless webservice, connecting to specific replicas is really
  not great"* — hence the parking.

## What happens today (measured and read, 2026-11-19)

**The problem, measured.** Debug build, debug world, RX 6700 XT, a temporary probe that pressed End
adventure 20 s into play at region (-5,-3). The server moved the body to `SPAWN_REGION` (0,0); the client
then loaded 9 near regions (snapshots ~0.5–1.2 MB, decoded in 160–320 ms on the network task) and 88 LOD
roots, with ~10 000 chunks queued for remeshing — all behind the full-screen map. Frame rate: 8–30 fps
for ~5 s, then a ~4 s stall with frames up to 1 s (no log event explains the stall). Slint's own render
was < 1 ms per second; the world cameras are inactive while the frontend owns the screen
(`commander::yield_commander_camera`, `first_person.rs`). After loading, a steady 60 fps.

**A session is always a body.** A fresh player is created at `SPAWN_REGION` (`world_manager/mod.rs`
`place_player`); a returning one is restored at its record's region; End adventure moves the body to
`SPAWN_REGION` and releases the claim (`world_link.rs` `end_adventure`). The selection screen is the
in-world game UI drawn while `InPlay` is false (`session.rs` `frontend_owns` / `arrival_map_shown`,
`slint_overlay.rs` `arrival_map`, `map_modal`).

**Where the selection screen's data comes from** (all host → client packets today):
- `WorldMap`: sent once at admit (`mod.rs` ~3750) from the host's `world_map`, read from the store's
  `forged_world_map` row (`world-store/src/forged.rs`), built once at forge time. The coordinator process
  reads the forged world at start (`roles.rs` ~561) but keeps only a `SocietyGrid` built from it — the map
  itself would have to be retained.
- `Calendar`, `Sites`, `Civs`: from the host's `world_view`, which the coordinator pushes
  (`CoordMsg::View` / `Calendar` / `Sites` / `SiteMarkers`, `world_link.rs` ~653–686) from its own
  `History` (`coordinator/mod.rs` `push_view`).
- `Legends`, `PersonInfo`: the host only relays `WorldRequest`s; the coordinator answers from `History`
  (`coordinator/mod.rs` ~1913).
- `AdventureResult`, claims: the coordinator's `start_adventure` (`coordinator/mod.rs` ~1972) grants the
  claim; today the host's report is the truth on reconcile (`player_online`, ~1752).
So everything the screen needs except the map is already the coordinator's.

**The join (jwt).** Client `JoinRequest { refresh_token, world }` → login `join_inner` (bumps the session
epoch) → `LoginToHost::SpawnPlayer` to a coordinator replica → `login_order` → that replica's gate host
(`gate_host_here`; none → `NoGate`, login tries its next replica link; nobody anywhere → `Starting` and
the fleet adds a host, Elastic 4) → the gate reads the player's record region → `place_spawn` → the
holding host acks with its endpoint → `JoinTicket { jwt, quic, webtransport, cert_hash }` → the client
connects to the host, which admits the JWT against the pending order (`auth_edge.rs`). The join JWT
lives `JOIN_JWT_TTL_SECS` = 300 s. No site travels anywhere in this path today.

**Single-player** (role `all`, `VOXEL_AUTH=local`): `local_host.rs` starts the server with
`VOXEL_SINGLE_PLAYER=1`; the client connects straight to its QUIC port with `Credential::Local`; no login
service; the coordinator is a thread in the same process.

**StartAdventure today**: host checks → `WorldRequest::StartAdventure` → coordinator grants the claim →
the host `move_client`s the body to the site's landing (or `move_to_peer` to the owning host; the client
follows by `HomeHop`).

**The client** cannot run without a home region: `PlayerRegion(None, _)` is treated as (0,0)
(`instance.rs` ~2469), and `AppState::InWorld` is entered on the first `SetPlayer` (`session.rs`).

**The coordinator** has no client-facing listener (host link 6480, login link 6490, health 9100); on k8s
it is a headless ClusterIP Service (`deploy/modules/world/coordinator.tf`). Hosts have NodePorts.

**Replica topology (as of Elastic 4, verified in code).** A host's dialer takes the first replica that
answers and keeps that one link (`svc-link/src/lib.rs` `dial_loop`); the coordinator records one replica
per host (`world_cut.rs` `attachments`); messages for a host on another replica go through the `outbox`
table (`coordinator/mod.rs` `route_out`); login orders reach only a host on the receiving replica
(`gate_host_here`). This per-replica routing is what the owner wants gone.

## The intended shape (agreed direction; details to be redone)

- **No world behind the screen.** While the selection screen is up the client has no world session: no
  host connection, no body, no regions, no LOD. Ending an adventure returns there. Browsing needs no host
  (a world at zero hosts is browsable; a host starts only for a pick or a resume).
- **A lobby connection to the coordinator** (QUIC + WebTransport, one Service over the replicas) serves
  the world map, civs, sites, calendar, legends, person info and the player's claim, with live pushes.
- **Pick a site** → the coordinator grants the claim, has the host place the character at the site's
  landing (the record's holder hands it to the site's owner when they differ — the path `place_player`
  already takes for a record whose land moved), and gives the client that host's endpoint and ticket.
- **The character between adventures** (the owner's game design, `docs/game-design.md` §1, §3.2: one
  character, inventory and skills persist): End adventure parks the record where the body stands and the
  body leaves the world; the next adventure brings the same record to the new site. A player with no
  record is created at the site's landing. `SPAWN_REGION` stops being a place bodies stand.
- **Resume**: a player still holding a claim when the lobby admits them goes straight back into play.
- **Single-player** uses the same lobby: the coordinator is in the local process.
- **Client**: a new app state outside the world session for the selection screen (frontend camera only);
  `InWorld` then always means in play; debug and measuring launches (`client debug world [--site N]`,
  `VOXEL_SKIP_MENU`, autopilot, render bench, `run-two`, `go-nogo-r3`) pick their site automatically.
- **Deployment**: the coordinator gets client ports and the game certificate (`VOXEL_CLIENT_CERT`), one
  NodePort Service over every replica; kube-proxy keeps a QUIC flow on one pod, and a dropped connection
  simply reconnects to any replica (it must lose nothing — which the stateless coordinator guarantees).

## Open, to be redesigned on the stateless coordinator

A builder check of the first draft raised four questions, all from the per-replica routing above. The
first draft answered them by putting placements, session epochs and kicks in the coordinator row and
routing through the outbox; that answer is **superseded** — redo each against the stateless coordinator:

1. **Getting a placement order to the right host and its answer back** to the client's lobby connection,
   whichever replicas they are on.
2. **Who is the truth about the adventure claim** when no host holds the player (the coordinator), and
   how End adventure's release and a reconnect on another replica stay consistent (never resume an ended
   adventure).
3. **What the client shows the host after a pick**: the 300 s join JWT can expire while browsing — a
   coordinator-minted ticket for the placement is the likely answer.
4. **"Newest session wins" and login kicks** across lobby connections on any replica and the host
   connection that follows.

## Done already (2026-11-19, uncommitted at the time of writing)

- The world map's Play button and the client's whole Play path are gone (`UiAction::Play`,
  `PlayPending`, `PendingPlay`, `ClientRequest::PlayRegion`, the refusal display,
  `ClientUpdateEvent::PlayRefused`). The wire `ClientPacket::PlayRegion` / `ServerPacket::PlayRefused`
  and the server's `play_region` remain.
- The start screen shows **Main menu** in the Close slot while the map is modal (`world-map.slint`).
- The slowness after End adventure is **not fixed**; it goes away with this work.
