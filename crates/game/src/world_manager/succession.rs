//! A succession (R4): the survivor's half (plan Task 8), then the heir's (Task 9). Host
//! `predecessor` (process `predecessor_boot`) was declared dead and all its super-regions
//! are `heir`'s now ([`crate::CoordMsg::Succession`]). **The survivor's half:** every live
//! host named a survivor — the heir among them — answers it, in order:
//!
//! 1. **Base.** Its peer journal counts from `base_cut`, or it waits (the `CutDone` of that
//!    cut is on its way: a report against another base would mix two cuts' traffic). Never
//!    reported against another base; a wait is logged once per [`SUCCESSION_WAIT_LOG_MS`],
//!    and a host that never gets there is the coordinator's deadline's to roll back.
//! 2. **Close.** The link to the predecessor is closed and a hello of `predecessor_boot` is
//!    refused from now on: a zombie of it exchanges nothing. A hello of ANOTHER boot of that
//!    ordinal is a new host (fresh numbering both ways, R3's boot rule).
//! 3. **Report** to the heir ([`Reliable::SuccessionReport`]; the heir files its own): the
//!    journal against the predecessor ([`crate::PeerJournal::take`]) and, as `owed`, the
//!    intents and replies this host still owed it — its session's unacknowledged ones,
//!    `peer_waiting`, what a leave held back — in order, unwrapped. A transfer or move owed
//!    is NOT in `owed`: the journal recorded it at queue time (`Sent`); what a leave held back
//!    is journalled now, so it is reported the same way. Everything else owed is dropped: a
//!    `SpawnOrder` is answered `SpawnAck { ok: false }` to its origin; `ObserverOrder`,
//!    `Kick`, `KickAck`, `SessionEnded`, `PlayerParked`, `CutMark`, `Owns` are forgotten (a
//!    kick waiting for the predecessor's acknowledgement no longer does).
//! 4. **Re-route.** The directory gives every `moved` row to the heir (the predecessor is
//!    no longer live); the predecessor's running regions end here (`HaloSourceGone`, marked
//!    stale for the store's re-seed); its observer tickets, endpoint, `left_peers` entry,
//!    `early_marks`, `ClaimWanted` throttle, sync and cut-view marks go; a player handed to
//!    it (`departed`, `handed`) points at the heir when a `Sent` entry of the report carries
//!    it (the heir places it from the journal; on the heir itself it points nowhere) and is
//!    forgotten otherwise (the store's record places it at its next login); a player it handed here and whose session has not come
//!    is parked without telling it.
//! 5. **Answer** [`crate::HostMsg::SuccessionApplied`]. From here a region of a moved
//!    super-region is an ordinary foreign region of the heir — reachable once the heir
//!    announces it — and what this host sends the heir is ordinary traffic between
//!    participants (`world_cut_wrap` unchanged).
//!
//! **A `Leave` succession** (R4 drain, Task 11) takes the same steps: the predecessor left
//! in a committed leave cut, so its journal entries are `Sent` ones only — what the leave
//! pair held back and what was sent since, all held in the session for its next boot — and
//! the session, emptied, is released for that next boot. Its next boot registering races
//! the declaration; the store decides (the declaration needs the clean row of that very boot,
//! a registration makes it dirty under the same `world` row lock), and a survivor mirrors
//! that order: a new boot's hello releases the held session only once that boot shows it
//! holds the land it left with (its `Owns`, `world_cut.rs` `peer_hello_boot`) — which it can
//! only if it re-claimed before any declaration (a later declaration is then refused,
//! `Gone`). A new boot that owns nothing registered after the declaration: its order comes,
//! and what was held goes to the heir in the report, the new boot renumbered from 1 — one
//! copy either way. A leaver that held NO land (as this host's directory gave it when the
//! leave BEGAN — the leave pair's note, taken before any declaration could move it) is
//! released at once: no succession can owe its heir anything. A host that booted after the
//! leave notes it from the store and its first directory read; if it registered before the
//! declaration and read after it, the order finds the session released already — nothing
//! of the departed boot is held there, an empty report (`apply_leave_overtaken`).
//!
//! **An heir that is no survivor** (a succession `worldctl retire-host` opened while no host
//! ran: `survivors` empty). The world was at its last cut as a whole, so nothing crossed that
//! a journal would have to resolve; the heir booted after the retirement, owning the land
//! from its first directory read (its boot re-claimed it). It takes the same steps with an
//! empty report of its own (no mask: the land is already its own), resolves at once (no
//! survivor to wait for), reads the predecessor's in-flight rows (`adopts`), answers
//! `SuccessionAdopted`, and its first committed cut settles it. A boot that ran beside the
//! dead boot (it met its hello, or journals traffic with it) never adopts such an order: what
//! it exchanged would be accounted nowhere — the coordinator's deadline rolls back instead.
//!
//! A run is forgotten at [`crate::CoordMsg::SuccessionDone`]; until then an `Attach` lists
//! it as answered (a restarted coordinator counts it again), and the heir's as adopted once
//! it is.
//!
//! **The heir's half.** The heir holds what it inherits masked from the order on — before
//! step 1's wait, so a directory read that already shows the moved rows starts nothing —
//! ([`WorldManager::inheriting`]: `owner_of` answers `Unknown` for it, so nothing of it
//! starts, arrivals and placements wait). Once a report of EVERY survivor is in (its own
//! included; "wait, never guess": a missing one blocks — the coordinator's deadline turns
//! that into a rollback — and a report against another base cut is never resolved) it
//! makes the drop set and asks the server for the store
//! ([`WorldManager::take_adoption_wanted`]); the server's answer
//! ([`WorldManager::adopted_rows`]) runs, in this order:
//!
//! 1. **Resolve.** Every entry grouped by subject; the highest stamp decides — an entity's
//!    transfer stamp, a player's hop (`peer/journal.rs` `subjects`: the one order over every
//!    host, whatever their clocks) — (two events of
//!    one subject never share a stamp; if they do, a `Received` one wins — delivering too
//!    could make two). `Received` latest: a survivor's copy lives on — nothing delivered.
//!    `Sent` latest: the predecessor held it — its journalled message is delivered. The
//!    **drop set** is every entity any entry names: none of them is taken from the
//!    predecessor's store (it lives elsewhere, or comes from the journal). A body that hands
//!    a player over is never delivered as an entity: its player's subject decides.
//! 2. **Adopt rows.** The predecessor's in-flight rows of the base cut (the server read
//!    them under the fence of what this host holds now): those of a dropped entity are
//!    discarded, the rest put back on their way ([`WorldManager::restore_in_flight`]).
//! 3. **Deliver.** A plain entity's bundle arrives through `accept_arrival` like any crossing
//!    from another host (its region applies it at its next slot, never on receipt: the
//!    slot-order rule); a target that moved on to another host is sent there. A person's
//!    bundle is NOT delivered (the history's lists respawn people as their region starts —
//!    delivering too would make two): this host, the receiving host now, reports the
//!    crossing absorbed (`PersonCrossed { delivered: false }`), before any region of the
//!    land starts, so a person whose crossing died with the predecessor is abstract, not
//!    lost in a region that no longer holds their body. A player handed or moved to the
//!    predecessor is parked here from the hand-off's record, at its hop, never `departed`;
//!    no session will come (the client's home was the dead host, or still the handing
//!    survivor), so the client is told: its session here is refused
//!    (`PLAYER_PARKED_ELSEWHERE`), the reporting survivor ends its own
//!    (`Reliable::PlayerParked`), the login session is reported over. A player whose latest
//!    event is another survivor's `Received` lives on there: this host points at that
//!    survivor (`departed`) in the predecessor's stead, so a login is sent on, never placed
//!    from the store's older record in the inherited land. Then the `owed` intents
//!    and replies, in each sender's order, as if that sender's session had brought them.
//! 4. **Load.** Every inherited region the store holds a dropped entity in (`located`), and
//!    every one an owed intent or reply goes to, starts now — the drop set left out of its
//!    store state ([`RegionSpawner::forget_entities`]) — pinned (`succession_pinned`) so it
//!    cannot park before the settle cut writes it. A pinned region whose load fails (or that
//!    is unavailable now) stays pinned and starts again at its retry (`retry_unavailable`);
//!    a reply for it waits in its mailbox; and no cut part of this host is taken while a
//!    pinned region is not in it (`try_complete_cut`: the part fails, the coordinator aborts
//!    the cut, a later one is taken once the region runs) — "wait, never guess". (The
//!    store's stale-row guard refuses a part missing one: the safety net, not the mechanism.)
//! 5. **Answer** [`crate::HostMsg::SuccessionAdopted`]. From here until
//!    `SuccessionDone { settled: true }` every cut part of this host adopts the predecessor
//!    ([`crate::WorldCut::adopts`]: the store replaces its in-flight rows with this part's) —
//!    the coordinator pairs that cut's `settles` with it. `SuccessionDone` unpins the
//!    regions and lifts the drop set (the settle cut rewrote every stored region holding
//!    one of them).
//!
//! **Where the mask is lifted.** `adopt_inherited` (lift the mask, flush what waited for
//! the land, retry held spawn orders) runs AFTER the drop set is installed in the spawner
//! and the rows are filtered, after the people are reported absorbed and after the
//! journalled players are parked, but BEFORE the rows, bundles and owed messages go on: the
//! flush may start an inherited region, which must already leave the drop set out; a person
//! must be reported before their region runs (the history would embody them in it); a spawn
//! order held while the land was masked must find the journal's parked record (the newer
//! one), not place the player from the store's base-cut record; and everything delivered
//! afterwards finds the land owned (`owns`), so nothing of it waits for an owner or bounces.
//! A report that comes after the resolution is dropped: the drop set and the resolution are
//! made from the same reports.
//!
//! **Spawn orders wait on the heir.** From the moment the heir applies a succession (its
//! mask goes up) until it has adopted it — every one, for an heir of several — EVERY spawn
//! order is held (`placement.rs`, the held-spawn queue), not only those aimed at the
//! inherited land: until the reports are resolved the heir cannot know which players a
//! survivor's journal holds a newer record of, and placing one from the store's base-cut
//! record could pick the stale copy. The held orders are retried right after the adoption
//! (a player parked from a journal is then found here) or when the succession settles, and
//! on every peer pump meanwhile (`retry_held_spawns`): an order still held
//! `SPAWN_SYNC_WAIT_MS` after it came is refused (`SpawnAck { ok: false }`, the client
//! retries its join). An abandoned succession (`SuccessionDone { settled: false }`) lifts the
//! mask, so a held order may be placed on this host at its next retry although a rollback
//! follows (`Generation`): this host exits without a save and the session is refused like any
//! other at a rollback — the player's stored record is untouched, so it is placed once, at
//! the client's next join, never twice. Other survivors are unchanged: their `departed`
//! pointers send a moved player's order on to the heir, which holds it.
//!
//! **Who accounts for what the heir delivers — the three sender cases** (`world_cut.rs`,
//! re-derived for a succession; no world cut runs from the declaration until the heir's
//! `SuccessionAdopted`, and every cut pending with the predecessor was aborted before it):
//! (i) a message the predecessor sent and a survivor received is that survivor's — in its
//!     snapshot of the settle cut if applied by its tick, else its row (`Received` latest:
//!     the heir drops it from the predecessor's store state and rows);
//! (ii) a journalled bundle, player or owed message the heir delivers is the heir's own —
//!     it went bare, no cut wraps it (`inbound_accounted` is none): in its snapshot of the
//!     settle cut if applied by its tick, else its row (an arrival waiting, a parked record,
//!     a mailbox entry);
//! (iii) a message a survivor sends the heir for a moved region after its `Applied` is an
//!     ordinary state message between participants and follows `world_cut_wrap`
//!     unchanged (it waits on the heir while the land is masked: the heir's row).

use super::*;
use crate::HostId;
use crate::{JournalEntry, JournalEvent, Reliable, Subject, Succession, SuccessionId};

/// At most one log line per this many ms about successions waiting for their base cut.
pub const SUCCESSION_WAIT_LOG_MS: u64 = 5_000;

/// A succession this host was ordered to answer.
#[derive(Debug, Clone)]
pub(super) struct SurvivorRun {
    pub order: Succession,
    /// Reported and re-routed (`SuccessionApplied` sent).
    pub applied: bool,
}

/// A survivor's report as the heir holds it ([`Reliable::SuccessionReport`]).
#[derive(Debug, Clone, PartialEq)]
pub struct SuccessionReportIn {
    pub base_cut: CutId,
    pub entries: Vec<JournalEntry>,
    pub owed: Vec<Reliable>,
}

/// Where an heir's adoption of a succession stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AdoptionState {
    /// Every report is in: the store read is wanted ([`WorldManager::take_adoption_wanted`]).
    Wanted,
    /// The server reads the store.
    Asked,
    /// Adopted (`SuccessionAdopted` sent): this host's cut parts adopt the predecessor
    /// until the succession is over.
    Adopted,
}

/// An heir's adoption of one succession.
#[derive(Debug, Clone)]
pub(super) struct Adoption {
    pub predecessor: HostId,
    /// Every entity a survivor's journal names: never taken from the predecessor's store.
    pub drop: BTreeSet<crate::EntityId>,
    pub state: AdoptionState,
    /// The regions started for it (pinned until the succession is over).
    pub pinned: BTreeSet<RegionCoords>,
}

/// What the survivors' journals resolve to (the module docs' step 1).
#[derive(Debug, Default)]
struct Resolved {
    /// Entries read.
    entries: usize,
    /// `Received` entries (a leave's reports should have none).
    received: usize,
    /// Subjects with two events at one stamp.
    ties: usize,
    /// Entity bundles to deliver: `(target, stamp, bundle)`, oldest stamp first.
    bundles: Vec<(RegionCoords, crate::Tick, crate::EntityBundle)>,
    /// People's bundles (never delivered): `(target, bundle)`.
    people: Vec<(RegionCoords, crate::EntityBundle)>,
    /// Players to park: the survivor that reported the entry, the hand-off and the record,
    /// oldest first.
    players: Vec<(HostId, crate::PlayerHandoff, PlayerRecord)>,
    /// Players a survivor holds (their latest event is that survivor's `Received`): the
    /// player and the survivor.
    held_by: Vec<(PlayerId, HostId)>,
}

/// Step 1 of the heir's half: every entry of every report grouped by subject; the entry
/// with the highest stamp decides (two events of one subject never share a stamp — a
/// transfer is applied at stamp + 1; if two ever do, a `Received` one wins: a survivor
/// holds the subject, and delivering too would make two). `Received` latest: nothing to
/// deliver (the survivor's copy lives on). `Sent` latest: the message is delivered — an
/// entity's bundle, a person's never (absorbed), a player's as a record to park; a body
/// that hands a player over is never delivered as an entity (its player's subject decides).
fn resolve(reports: &BTreeMap<HostId, SuccessionReportIn>) -> Resolved {
    let mut out = Resolved::default();
    // subject → (stamp, sent, reporter, index)
    let mut latest: BTreeMap<Subject, (crate::Tick, bool, HostId, usize)> = BTreeMap::new();
    for (from, report) in reports {
        for (i, e) in report.entries.iter().enumerate() {
            out.entries += 1;
            let sent = matches!(e.event, JournalEvent::Sent(_));
            out.received += usize::from(!sent);
            let replace = match latest.get(&e.subject) {
                None => true,
                Some(&(stamp, _, _, _)) if e.stamp > stamp => true,
                Some(&(stamp, was_sent, _, _)) if e.stamp == stamp => {
                    out.ties += 1;
                    was_sent && !sent
                }
                Some(_) => false,
            };
            if replace {
                latest.insert(e.subject, (e.stamp, sent, *from, i));
            }
        }
    }
    let mut players = Vec::new();
    for (subject, (stamp, sent, from, i)) in latest {
        if !sent {
            if let Subject::Player(player) = subject {
                out.held_by.push((player, from));
            }
            continue;
        }
        let JournalEvent::Sent(msg) = &reports[&from].entries[i].event else { continue };
        match (subject, msg.as_ref()) {
            // A body handing a player over: the player's subject decides.
            (Subject::Entity(_), Reliable::Transfer { player: Some(_), .. }) => {}
            (Subject::Entity(_), Reliable::Transfer { target, stamp, bundle, player: None }) => {
                if matches!(bundle.kind, EntityKind::Person { .. }) {
                    out.people.push((*target, (**bundle).clone()));
                } else {
                    out.bundles.push((*target, *stamp, (**bundle).clone()));
                }
            }
            (Subject::Player(_), Reliable::Transfer { target, bundle, player: Some(h), .. }) => {
                match peers::handoff_record(*target, bundle, h) {
                    Some(record) => players.push((stamp, from, h.clone(), record)),
                    None => log::error!("[server] a journalled hand-off of player {} carries no player", h.player.0),
                }
            }
            (Subject::Player(_), Reliable::PlayerMove { handoff, record, .. }) => {
                players.push((stamp, from, handoff.clone(), (**record).clone()));
            }
            (subject, _) => log::error!("[server] a journal entry about {subject:?} that is not its message; skipped"),
        }
    }
    out.bundles.sort_by_key(|(_, stamp, b)| (*stamp, b.id));
    players.sort_by_key(|(stamp, _, h, _)| (*stamp, h.player));
    out.players = players.into_iter().map(|(_, from, h, r)| (from, h, r)).collect();
    out
}


// --- forgetting a host (Elastic 1 Task 4) ----------------------------------------------------

/// THE list of the manager's fields by how they hold a host (one list: [`WorldManager::forget_host`]
/// and [`WorldManager::mentions`] are both generated from it). Every field of `WorldManager`
/// is named here exactly once — the generated code destructures the struct with no `..` — so
/// a new field does not compile until it is classified: per host (one of the kinds) or
/// `other` (holds no host, or holds one only for a moment that a forgotten host never reaches:
/// `world_cut` names participants of a cut that is decided before any host is forgotten;
/// `spawn_held` / `spawn_here` / `kicks_in` are answered within `SPAWN_SYNC_WAIT_MS`;
/// `entry_host` is the login link's; `cut_refused_for` is a test-support record of a refusal; `successions`, `succession_reports`, `adoptions` end at
/// `SuccessionDone`, where the predecessor is forgotten; `forgotten_hosts` is the tombstone).
/// Kinds: `key_map` / `key_set` keyed by the host; `pair_set` of `(host, _)`; `value_map`
/// whose values are hosts; `value_first` / `value_second` whose value tuples hold the host
/// first / second (`value_second` is re-pointed at this host, not dropped: a player handed
/// here by the forgotten host still gets its session grace); `key_second` keyed by
/// `(_, host)`; `value_sets` of host sets; `journal` the peer journal.
macro_rules! host_fields {
    ($callback:ident) => {
        $callback! {
            key_map: [
            peers, peer_waiting, peer_queued, peer_endpoints, peer_boots, left_peers, returning_peers,
            left_land, leave_pairs, count_pairs, early_marks, succeeded_peers
            ],
            key_set: [peer_synced, pair_owns_seen, left_land_unread, unsaved_traffic],
            pair_set: [dead_boots, peer_in_source_view],
            value_map: [foreign_running, redirected, departed],
            value_first: [handed, claim_wanted_sent],
            key_second: [observer_tickets],
            value_second: [awaiting_session],
            value_sets: [kicks_pending],
            journal: [journal],
            other: [
            spawner, generator, regions, parked, terrain_cache, pristine_terrain, pristine_terrain_max,
            sessions, homes, world_map, route_map, pending_events, world, world_view, spawn_waiting,
            world_live, world_attach, attach_epoch, world_generation, host_generation, world_boot,
            next_request_seq, settled_below, pending_requests, expired_requests, claim_seq, claim_changes,
            claim_reported, player_hop, hop_renumbers, outbox, world_request_timeout_ms, world_counters,
            site_regions, society_grid, world_ms, world_notices, fortress_of, adventurer_of, player_of,
            parking, moving, parked_players, awaiting_park, deferred_packets, clock_ms, unavailable, ownership,
            creating, out, region_out_send, lod_subscribers, lod_cache, lod_cache_max, lod_access_counter,
            lod_in_flight, lod_submitted_at, perf, lod_pending_terrain, lod_tile_waiting, lod_builder,
            lod_results, directive_stamps, in_flight, pending_cut, ready_cut, outstanding_cuts, uncommitted,
            uncommitted_seq, cut_lead, next_cut_id, cut_order, silent_stopping, last_cut_caught,
            stored_regions, locations, in_flight_intents, intent_mailbox, pending_arrivals, arrivals_despawned,
            recently_despawned, despawn_warned, intent_replies_lost, parked_bodies, items_destroyed,
            incarnation, incarnation_seen, next_incarnation, unowned_waiting, claims_wanted, claim_asks,
            land_wanted, land_wanted_sent, land_scanned_ms, directory_wanted, journal_always,
            peer_dropped_for_dead, peer_state_to_forgotten, cut_refused_for, forgotten_hosts, foreign_halo_stale, higher_generation, endpoint,
            asked_elsewhere_answered_ms, observer_orders, nonces, observer_revocations,
            foreign_input_warned_ms, unseeded_warned, world_cut, world_cut_seen, wrapper_seen, marks_queued,
            leave_requested, leaving_cut, leave_outcome, leave_decided, leave_parked, world_cut_done,
            inbound_accounted, entry_host, logins, spawn_held, spawn_here, kicks_in, kicked, login_out,
            spawn_pins, kick_warned_ms, successions, succession_reports, succession_wait_logged_ms, inheriting,
            successions_done, adoptions, succession_pinned, succession_seam_events
            ],
        }
    };
}

/// [`host_fields`]' callback: `forget_fields`, every per-host field without the host.
macro_rules! forget_fields {
    (
        key_map: [$($km:ident),* $(,)?],
        key_set: [$($ks:ident),* $(,)?],
        pair_set: [$($ps:ident),* $(,)?],
        value_map: [$($vm:ident),* $(,)?],
        value_first: [$($vf:ident),* $(,)?],
        key_second: [$($k2:ident),* $(,)?],
        value_second: [$($v2:ident),* $(,)?],
        value_sets: [$($vs:ident),* $(,)?],
        journal: [$j:ident],
        other: [$($o:ident),* $(,)?],
    ) => {
        /// Every per-host field of [`host_fields`] forgets `h` (`value_second` points at `me`);
        /// the journal is [`WorldManager::forget_host`]'s (it may refuse).
        fn forget_fields(&mut self, h: HostId, me: HostId) {
            let WorldManager { $($km,)* $($ks,)* $($ps,)* $($vm,)* $($vf,)* $($k2,)* $($v2,)* $($vs,)* $j: _, $($o: _,)* } = self;
            $( $km.remove(&h); )*
            $( $ks.remove(&h); )*
            $( $ps.retain(|(o, _)| *o != h); )*
            $( $vm.retain(|_, o| *o != h); )*
            $( $vf.retain(|_, v| v.0 != h); )*
            $( $k2.retain(|k, _| k.1 != h); )*
            $( for v in $v2.values_mut() { if v.1 == h { v.1 = me; } } )*
            $( for v in $vs.values_mut() { v.remove(&h); } $vs.retain(|_, v| !v.is_empty()); )*
        }
    };
}

/// [`host_fields`]' callback: `mentions`, the per-host fields that still hold a host.
macro_rules! mention_fields {
    (
        key_map: [$($km:ident),* $(,)?],
        key_set: [$($ks:ident),* $(,)?],
        pair_set: [$($ps:ident),* $(,)?],
        value_map: [$($vm:ident),* $(,)?],
        value_first: [$($vf:ident),* $(,)?],
        key_second: [$($k2:ident),* $(,)?],
        value_second: [$($v2:ident),* $(,)?],
        value_sets: [$($vs:ident),* $(,)?],
        journal: [$j:ident],
        other: [$($o:ident),* $(,)?],
    ) => {
        /// Test support: the fields of [`host_fields`] that still hold host `h` (by name): empty
        /// once it was forgotten ([`Self::forget_host`]).
        #[doc(hidden)]
        pub fn mentions(&self, h: HostId) -> Vec<&'static str> {
            let WorldManager { $($km,)* $($ks,)* $($ps,)* $($vm,)* $($vf,)* $($k2,)* $($v2,)* $($vs,)* $j, $($o: _,)* } = self;
            let mut out = Vec::new();
            $( if $km.contains_key(&h) { out.push(stringify!($km)); } )*
            $( if $ks.contains(&h) { out.push(stringify!($ks)); } )*
            $( if $ps.iter().any(|(o, _)| *o == h) { out.push(stringify!($ps)); } )*
            $( if $vm.values().any(|o| *o == h) { out.push(stringify!($vm)); } )*
            $( if $vf.values().any(|v| v.0 == h) { out.push(stringify!($vf)); } )*
            $( if $k2.keys().any(|k| k.1 == h) { out.push(stringify!($k2)); } )*
            $( if $v2.values().any(|v| v.1 == h) { out.push(stringify!($v2)); } )*
            $( if $vs.values().any(|v| v.contains(&h)) { out.push(stringify!($vs)); } )*
            if $j.mentions(h) { out.push(stringify!($j)); }
            out
        }
    };
}

impl<S: RegionSpawner> WorldManager<S> {
    host_fields!(forget_fields);
    host_fields!(mention_fields);

    /// Host `host` is gone for good (Elastic 1 Task 4): every field of [`host_fields`] forgets
    /// it, its session and link are dropped (a dialer to it stops: the server's dialing thread
    /// ends when its link is dropped), and it is never linked or written to again (an id is
    /// never issued twice). Called for the predecessor of a SETTLED succession (at
    /// `SuccessionDone`: the settle cut committed, the reports are no longer read) and for a
    /// host that left cleanly holding no land (`world_cut.rs` `forget_landless_leavers`: no
    /// `Leave` succession can owe its heir anything of it). Refused — nothing forgotten — while
    /// the peer journal still needs it ([`crate::PeerJournal::forget`]).
    ///
    /// **What is dropped** (counted, `peer_dropped_for_dead=`): what this host still owed it —
    /// its session's unacknowledged messages, `peer_waiting`, what a leave or count pair held.
    /// For the predecessor of a settled succession nothing state-bearing is left there: the
    /// succession took its session's messages (`apply_succession`: transfers and moves are
    /// `Sent` journal entries the heir resolved, intents and replies went to the heir as
    /// `owed`); for a landless leaver no state message is addressed to it (it owns no region).
    /// What may be: a `SpawnOrder` / `SpawnAck` (the login service's client retries its join),
    /// a `Kick` (durable in the login service until acknowledged — it is acknowledged here, as
    /// a succession does: the forgotten host holds nobody — and sent again by the login service
    /// otherwise), `KickAck`, `SessionEnded`, `PlayerParked`, `ObserverOrder`, `Owns`,
    /// `CutMark` (no cut is pending with it: it is outside every later cut).
    pub fn forget_host(&mut self, host: HostId) -> Result<(), crate::ForgetRefused> {
        let me = self.ordinal();
        if host == me {
            return Err(crate::ForgetRefused { peer: host, why: "it is this host".into() });
        }
        self.journal.forget(host)?;
        let mut dropped = 0usize;
        if let Some(peer) = self.peers.get_mut(&host) {
            dropped += peer.session.take_unreceived().len();
            peer.link.close();
        }
        dropped += self.peer_waiting.get(&host).map_or(0, Vec::len);
        dropped += self.leave_pairs.get(&host).map_or(0, |p| p.held.len());
        dropped += self.count_pairs.get(&host).map_or(0, |p| p.held.len());
        // A kick no longer waits for its acknowledgement.
        let mut acked = Vec::new();
        for (key, waiting) in self.kicks_pending.iter_mut() {
            if waiting.remove(&host) && waiting.is_empty() {
                acked.push(key.clone());
            }
        }
        for key in acked {
            self.kicks_pending.remove(&key);
            self.login_out.push(LoginOut::KickAck { player_token: key.0, session_epoch: key.1 });
        }
        self.forget_fields(host, me);
        self.forgotten_hosts.insert(host);
        self.peer_dropped_for_dead += dropped as u64;
        log::info!("[server] host {host} is forgotten ({dropped} message(s) owed to it dropped)");
        Ok(())
    }

    /// Whether host `host` was forgotten ([`Self::forget_host`]).
    pub fn is_forgotten(&self, host: HostId) -> bool {
        self.forgotten_hosts.contains(&host)
    }

    /// Messages to forgotten hosts dropped unsent, since start (`peer_dropped_for_dead=`).
    pub fn peer_dropped_for_dead(&self) -> u64 {
        self.peer_dropped_for_dead
    }

    /// State messages addressed to a forgotten host that no path could take, since start
    /// (`peer_state_to_forgotten=`; each one a bug, logged as an error).
    pub fn peer_state_to_forgotten(&self) -> u64 {
        self.peer_state_to_forgotten
    }
}

impl<S: RegionSpawner> WorldManager<S> {
    /// The coordinator's order of succession `s` ([`crate::CoordMsg::Succession`]): answered
    /// as the module docs say — now, or once this host's journal counts from its base cut.
    /// A repeated order (a resend after a reconnect) is answered again if it was applied.
    pub(super) fn succession_ordered(&mut self, s: Succession) {
        let me = self.ordinal();
        if let Some(run) = self.successions.get(&s.id) {
            if run.applied {
                log::debug!("[server] succession {}: ordered again; applied already", s.id.0);
                self.world_send(HostMsg::SuccessionApplied { id: s.id });
            }
            return;
        }
        if s.predecessor == me {
            log::error!(
                "[server] succession {}: THIS host ({me}) was declared dead (boot {:#x}); its writes are fenced",
                s.id.0,
                s.predecessor_boot
            );
            return;
        }
        if !s.survivors.contains(&me) {
            if s.heir != me || !s.survivors.is_empty() {
                log::warn!("[server] succession {}: this host is not among its survivors {:?}; ignored", s.id.0, s.survivors);
                return;
            }
            // The heir, no survivor: a succession opened while no host ran (`worldctl
            // retire-host`). Sound only for a boot that never ran beside the dead one — the
            // world is at its last cut as a whole, so there is no journal to resolve. A boot
            // that met it (a host that hung through the retirement and woke) holds traffic
            // nobody would account for: never adopted; the coordinator's deadline rolls back.
            let met = self.peer_boots.get(&s.predecessor) == Some(&s.predecessor_boot)
                || self.journal.entries(s.predecessor).next().is_some();
            if met {
                log::error!(
                    "[server] succession {}: this host is its heir but no survivor, and it ran beside host {}'s dead boot \
                     {:#x}: not adopted (the world rolls back at the succession's deadline)",
                    s.id.0,
                    s.predecessor,
                    s.predecessor_boot
                );
                return;
            }
            log::warn!(
                "[server] succession {}: host {} (boot {:#x}) was retired while no host ran; this host, booted since, is its \
                 heir: it adopts the land and in-flight rows from the store (base cut {})",
                s.id.0,
                s.predecessor,
                s.predecessor_boot,
                s.base_cut.0
            );
        }
        log::warn!(
            "[server] succession {}: host {} (boot {:#x}) is dead, heir {}, base cut {}",
            s.id.0,
            s.predecessor,
            s.predecessor_boot,
            s.heir,
            s.base_cut.0
        );
        let id = s.id;
        // The heir's mask goes up with the ORDER, not only when it is applied: a directory
        // read landing while this host still waits for its base cut already shows the moved
        // rows as its own, and nothing of them may start from the store before the
        // survivors' journals are resolved. (An heir that is no survivor has no journal to
        // wait for and owns the land from its boot: nothing is masked.)
        if s.heir == me && !s.survivors.is_empty() {
            for &(sr, _) in &s.moved {
                self.inheriting.insert(sr, id);
            }
        }
        self.successions.insert(id, SurvivorRun { order: s, applied: false });
        self.try_apply_succession(id);
    }

    /// Every succession not applied yet whose base cut this host's journal counts from now
    /// is applied; the others log their wait (at most once per [`SUCCESSION_WAIT_LOG_MS`]).
    pub(super) fn retry_successions(&mut self) {
        let waiting: Vec<SuccessionId> = self.successions.iter().filter(|(_, r)| !r.applied).map(|(id, _)| *id).collect();
        for id in waiting {
            self.try_apply_succession(id);
        }
    }

    fn try_apply_succession(&mut self, id: SuccessionId) {
        let Some(run) = self.successions.get(&id).filter(|r| !r.applied) else { return };
        let base = self.journal.base();
        if base != run.order.base_cut {
            let now = self.clock_ms;
            if self.succession_wait_logged_ms.is_none_or(|at| now.saturating_sub(at) >= SUCCESSION_WAIT_LOG_MS) {
                self.succession_wait_logged_ms = Some(now);
                log::warn!(
                    "[server] succession {}: this host's journal counts from cut {}, the succession from cut {}: waiting \
                     (for that cut's decision; the coordinator rolls the world back at its deadline)",
                    id.0,
                    base.0,
                    run.order.base_cut.0
                );
            }
            return;
        }
        let s = run.order.clone();
        self.apply_succession(&s);
        if let Some(run) = self.successions.get_mut(&id) {
            run.applied = true;
        }
        self.world_send(HostMsg::SuccessionApplied { id });
        // The heir: its own report is in now; the others may be too.
        self.try_resolve_succession(id);
    }

    /// Steps 2–4 of the module docs for succession `s` (its base cut matches).
    fn apply_succession(&mut self, s: &Succession) {
        let (pred, heir, me) = (s.predecessor, s.heir, self.ordinal());
        self.returning_peers.remove(&pred);
        let released = !self.left_peers.contains_key(&pred) && !self.leave_pairs.contains_key(&pred);
        if s.kind == crate::SuccessionKind::Leave && released && self.peer_boots.get(&pred).is_some_and(|b| *b != s.predecessor_boot) {
            // The held session was released to a new boot of the leaver before this order
            // came — only on a host that booted after the leave (`apply_leave_overtaken`):
            // nothing of the departed boot is held here. Anywhere else the session stays held
            // until this order (`peer_hello_boot`: the leave's land is noted when the leave
            // BEGINS, before any declaration could move it).
            return self.apply_leave_overtaken(s);
        }
        // 2. Close: nothing more of the dead boot is read, its hello is refused for good.
        self.dead_boots.insert((pred, s.predecessor_boot));
        self.succeeded_peers.insert(pred, s.predecessor_boot);
        let mut owed_raw: Vec<Reliable> = Vec::new();
        if let Some(peer) = self.peers.get_mut(&pred) {
            peer.session.refuse_boot(s.predecessor_boot);
            peer.link.close();
            owed_raw.extend(peer.session.take_unreceived());
            // A session a committed leave held (R3) owes nothing now: the next boot of that
            // ordinal is a new host — its hello renumbers from 1 and it is reachable again.
            peer.session.release();
        }
        owed_raw.extend(self.peer_waiting.remove(&pred).unwrap_or_default());
        // A world cut counted with the predecessor and not decided here yet (its `CutDone`
        // comes before any order on the coordinator's link; defensive): what it held back
        // is reported as a leave's is, and the session thaws for the next boot.
        let counted_held = match self.count_pairs.remove(&pred) {
            Some(pair) => {
                if let Some(peer) = self.peers.get_mut(&pred) {
                    peer.session.thaw();
                }
                pair.held
            }
            None => Vec::new(),
        };
        // What a leave held back was never queued (nor journalled): journalled now, so its
        // transfers and moves are `Sent` entries of the report like every other.
        // The pair ends here without its `CutDone` (an order applied before the decision of an
        // aborted leave cut came): undone as `end_leaves` undoes it — the session no longer
        // holds a next boot's hello for a leave that is over (nothing would release it), and
        // an `Owns` seen for the pair is forgotten.
        let leave_held = match self.leave_pairs.remove(&pred) {
            Some(pair) => {
                if let Some(peer) = self.peers.get_mut(&pred) {
                    peer.session.hold_on_new_boot(false);
                }
                self.pair_owns_seen.remove(&pred);
                pair.held
            }
            None => Vec::new(),
        };
        if !leave_held.is_empty() || !counted_held.is_empty() {
            let tick = self.spawner.world_tick();
            for msg in leave_held.into_iter().chain(counted_held) {
                let count = self.peer_queued.entry(pred).or_default();
                let qid = *count;
                *count += 1;
                self.journal.sent(pred, qid, &msg, None, tick);
                owed_raw.push(msg);
            }
        }
        self.peer_in_source_view.retain(|(o, _)| *o != pred);
        // 3. Report.
        let entries = self.journal.take(pred);
        let mut owed = Vec::new();
        let (mut dropped, mut refused_spawns) = (0usize, 0usize);
        for msg in owed_raw {
            match msg.inner().clone() {
                m @ Reliable::Intent(_) => owed.push(m),
                // A journal entry already (`Sent`): the heir resolves it.
                Reliable::Transfer { .. } | Reliable::PlayerMove { .. } => {}
                Reliable::SpawnOrder { origin, player_token, session_epoch, .. } => {
                    refused_spawns += 1;
                    self.spawn_answer(origin, player_token, session_epoch, false, None);
                }
                _ => dropped += 1,
            }
        }
        // A kick no longer waits for the predecessor's acknowledgement.
        let mut acked = Vec::new();
        for (key, waiting) in self.kicks_pending.iter_mut() {
            if waiting.remove(&pred) && waiting.is_empty() {
                acked.push(key.clone());
            }
        }
        for key in acked {
            self.kicks_pending.remove(&key);
            self.login_out.push(LoginOut::KickAck { player_token: key.0, session_epoch: key.1 });
        }
        let carried: BTreeSet<PlayerId> = entries
            .iter()
            .filter(|e| matches!(e.event, JournalEvent::Sent(_)))
            .filter_map(|e| match e.subject {
                Subject::Player(p) => Some(p),
                Subject::Entity(_) => None,
            })
            .collect();
        log::info!(
            "[server] succession {}: reporting {} journal entries and {} owed intents/replies to heir {heir} \
             ({dropped} other messages dropped, {refused_spawns} spawn orders refused)",
            s.id.0,
            entries.len(),
            owed.len()
        );
        let report = SuccessionReportIn { base_cut: s.base_cut, entries, owed };
        if heir == me {
            self.file_succession_report(me, s.id, report);
        } else {
            // A plain reliable message: no cut runs while the succession is unanswered, so it
            // is never wrapped (`world_cut_wrap` leaves a non-state message bare).
            let SuccessionReportIn { base_cut, entries, owed } = report;
            let msg = Reliable::SuccessionReport { id: s.id, base_cut, entries, owed };
            // A report the link cannot carry is never queued: the link would drop it at every
            // resend, stalling this peer session until the rollback. Not sent, the heir never
            // has every report, and the coordinator's deadline rolls the world back — the
            // failure is explicit, named here with its size.
            match crate::reliable_frame_bytes(&msg) {
                Some(bytes) if bytes <= crate::PEER_FRAME_MAX_BYTES => {
                    log::info!("[server] succession {}: the report to heir {heir} is {bytes} bytes on the link", s.id.0);
                    self.queue_to_peer(heir, msg);
                }
                bytes => log::error!(
                    "[server] succession {}: the report to heir {heir} is {} — over the peer link's {} byte frame \
                     limit; NOT sent: the heir waits for it and the coordinator's deadline rolls the world back",
                    s.id.0,
                    bytes.map_or_else(|| "not encodable".to_string(), |b| format!("{b} bytes")),
                    crate::PEER_FRAME_MAX_BYTES
                ),
            }
        }
        // 4. Re-route. The heir holds what it inherits until it adopts it (Task 9): its own
        // directory row is right (owner = itself, the new epoch), but `owner_of` answers
        // `Unknown` for those super-regions meanwhile — no region of them starts from the
        // store before the survivors' journals are resolved. An heir that is no survivor (an
        // offline retirement) booted owning that land and has no journal to wait for: the
        // store's state is the truth, its regions may run already — nothing is masked.
        if heir == me && !s.survivors.is_empty() {
            for &(sr, _) in &s.moved {
                self.inheriting.insert(sr, s.id);
            }
        }
        if let Some(directory) = self.ownership.0.directory_mut() {
            for &(sr, epoch) in &s.moved {
                directory.set(sr, Some(heir), epoch);
            }
            directory.note_dead(pred);
        } else if !s.moved.is_empty() {
            log::error!("[server] succession {}: this host keeps no directory; {} moved rows not taken", s.id.0, s.moved.len());
        }
        self.peer_lost(pred);
        self.peer_unsynced(pred);
        self.observer_tickets.retain(|(_, owner), _| *owner != pred);
        self.peer_endpoints.remove(&pred);
        self.left_peers.remove(&pred);
        self.left_land.remove(&pred);
        self.early_marks.remove(&pred);
        self.claim_wanted_sent.retain(|_, (to, _)| *to != pred);
        let mut departed_moved = 0usize;
        let mut departed_forgotten = 0usize;
        let pointing: Vec<PlayerId> = self.departed.iter().filter(|(_, h)| **h == pred).map(|(p, _)| *p).collect();
        for p in pointing {
            // The heir itself places a carried player from the journal (Task 9): it points
            // nowhere then (a player is never "handed to" the host that holds it).
            if carried.contains(&p) && heir != me {
                self.departed.insert(p, heir);
                departed_moved += 1;
            } else {
                self.departed.remove(&p);
                departed_forgotten += 1;
            }
        }
        let handed: Vec<PlayerId> = self.handed.iter().filter(|(_, (to, _, _))| *to == pred).map(|(p, _)| *p).collect();
        for p in handed {
            if carried.contains(&p) && heir != me {
                if let Some(entry) = self.handed.get_mut(&p) {
                    entry.0 = heir;
                }
            } else {
                self.handed.remove(&p);
            }
        }
        // Handed here by the predecessor, its session not come: nobody there to tell.
        for (_, from) in self.awaiting_session.values_mut() {
            if *from == pred {
                *from = me;
            }
        }
        if departed_moved + departed_forgotten > 0 {
            log::info!(
                "[server] succession {}: {departed_moved} departed player(s) now at heir {heir}, {departed_forgotten} \
                 forgotten (the store places them)",
                s.id.0
            );
        }
        // What waited for an owner of a moved super-region goes on (to the heir, or here).
        self.flush_unowned();
        // Spawn orders that waited for the predecessor's sync go on.
        self.retry_held_spawns();
        // Clients sent to the predecessor for a region are answered again (R4 Task 16): sent
        // to the heir, or — this host the heir — told to take it here once adopted.
        self.reanswer_redirected_to(pred);
    }

    /// [`Self::apply_succession`] of a `Leave` succession whose predecessor already came back
    /// here as another boot (its hello released the held session): an empty report (nothing
    /// this host holds is the departed boot's any more — what it held went to the new boot,
    /// which hands what it does not own on to its owner) and the directory's rows moved to
    /// the heir. The new boot keeps its session and stays live.
    fn apply_leave_overtaken(&mut self, s: &Succession) {
        let (pred, heir, me) = (s.predecessor, s.heir, self.ordinal());
        // Reached only by a host that BOOTED after the leave (`note_left_peers`), registered
        // before the declaration (it is a survivor) and read its directory after it (the land
        // shows the heir's, so the new boot's hello released the held session). That boot
        // never ran in the departed boot's time: its session holds no message of it, and its
        // directory sent that land's traffic to the heir from its first read — nothing it
        // holds is owed to the heir. No leave pair is left either: the order is applied only
        // once this host's journal counts from the base cut, i.e. after the leave cut's
        // `CutDone`, which ends the pair. So the report is empty, and the new boot's live
        // connection is left alone (refusing the departed boot on it would reset it one-sided).
        log::warn!(
            "[server] succession {}: host {pred} left as boot {:#x} and its next boot is linked here already (this host \
             booted after the leave): nothing of the departed boot is held here, an empty report",
            s.id.0,
            s.predecessor_boot
        );
        // A zombie of the departed boot is refused on any later connection (`set_peer`).
        self.dead_boots.insert((pred, s.predecessor_boot));
        self.left_peers.remove(&pred);
        self.left_land.remove(&pred);
        let report = SuccessionReportIn { base_cut: s.base_cut, entries: Vec::new(), owed: Vec::new() };
        if heir == me {
            self.file_succession_report(me, s.id, report);
            for &(sr, _) in &s.moved {
                self.inheriting.insert(sr, s.id);
            }
        } else {
            let msg = Reliable::SuccessionReport { id: s.id, base_cut: s.base_cut, entries: Vec::new(), owed: Vec::new() };
            self.queue_to_peer(heir, msg);
        }
        if let Some(directory) = self.ownership.0.directory_mut() {
            for &(sr, epoch) in &s.moved {
                directory.set(sr, Some(heir), epoch);
            }
        }
        self.flush_unowned();
        self.retry_held_spawns();
        self.reanswer_redirected_to(pred);
    }

    /// A survivor's report of succession `id` (`from`; this host is its heir): kept for the
    /// heir's resolution (Task 9).
    fn file_succession_report(&mut self, from: HostId, id: SuccessionId, report: SuccessionReportIn) {
        if self.successions_done.contains(&id) {
            log::warn!("[server] succession {}: host {from}'s report came after it was over; dropped", id.0);
            return;
        }
        if self.adoptions.contains_key(&id) {
            // The drop set was made from the reports as they were: never replaced.
            log::warn!("[server] succession {}: host {from}'s report came after the resolution; dropped", id.0);
            return;
        }
        if let Some(run) = self.successions.get(&id) {
            if run.order.heir != self.ordinal() {
                log::error!("[server] succession {}: host {from}'s report came here; the heir is {}", id.0, run.order.heir);
            }
        }
        log::info!(
            "[server] succession {}: host {from}'s report ({} entries, {} owed) filed",
            id.0,
            report.entries.len(),
            report.owed.len()
        );
        self.succession_reports.entry(id).or_default().insert(from, report);
        self.try_resolve_succession(id);
    }

    /// Host `from`'s [`Reliable::SuccessionReport`].
    pub(super) fn peer_succession_report(
        &mut self,
        from: HostId,
        id: SuccessionId,
        base_cut: CutId,
        entries: Vec<JournalEntry>,
        owed: Vec<Reliable>,
    ) {
        self.file_succession_report(from, id, SuccessionReportIn { base_cut, entries, owed });
    }

    /// The coordinator's [`crate::CoordMsg::SuccessionDone`]: the run and its reports end; an
    /// heir's adoption of it ends too — its regions unpinned, its drop set lifted from the
    /// spawner (the settle cut rewrote every stored region that held one of them).
    pub(super) fn succession_done(&mut self, id: SuccessionId, settled: bool) {
        let run = self.successions.remove(&id);
        let predecessor = run.as_ref().map(|r| r.order.predecessor).or_else(|| self.adoptions.get(&id).map(|a| a.predecessor));
        let reports = self.succession_reports.remove(&id);
        if run.is_some() || reports.is_some() {
            log::info!("[server] succession {} {}", id.0, if settled { "settled" } else { "abandoned" });
        }
        if let Some(adoption) = self.adoptions.remove(&id) {
            self.succession_pinned = self.adoptions.values().flat_map(|a| a.pinned.iter().copied()).collect();
            let unpinned: Vec<RegionCoords> =
                adoption.pinned.iter().copied().filter(|rc| !self.succession_pinned.contains(rc)).collect();
            let now_ms = self.clock_ms;
            for rc in unpinned {
                // Its keep-alive is the usual one from now on.
                self.refresh_keepalive(rc, now_ms);
            }
            let drop = self.adoption_drop_union();
            self.spawner.forget_entities(&drop);
        }
        // Nothing of it is held any more (Task 9 adopts before the settle; an abandoned one
        // comes with a rollback, and this process exits at its `Generation`).
        self.inheriting.retain(|_, s| *s != id);
        self.successions_done.insert(id);
        // Spawn orders held while this host had it to adopt go on. After an abandoned one
        // they are not retried here, but the next peer pump's `retry_held_spawns` (or their
        // `SPAWN_SYNC_WAIT_MS` refusal) answers them: a placement then lands on a host about
        // to exit for the rollback — no save, the stored record untouched — so the player is
        // placed once, at the client's next join (see the module docs).
        if settled {
            self.retry_held_spawns();
            // Its settle cut committed: nothing of the predecessor is read any more.
            if let Some(pred) = predecessor.filter(|p| !self.successions.values().any(|r| r.order.predecessor == *p)) {
                if let Err(e) = self.forget_host(pred) {
                    log::warn!("[server] succession {} settled, but host {pred} is not forgotten: {e}", id.0);
                }
            }
        }
    }

    /// The successions this host answered (`(id, adopted)`), for its `Attach`: `adopted`
    /// once this host, their heir, adopted them.
    pub(super) fn attach_successions(&self) -> Vec<(SuccessionId, bool)> {
        self.successions
            .iter()
            .filter(|(_, r)| r.applied)
            .map(|(id, _)| (*id, self.adoptions.get(id).is_some_and(|a| a.state == AdoptionState::Adopted)))
            .collect()
    }

    // --- The heir's half (Task 9) ---------------------------------------------------------

    /// Succession `id` (this host its heir, applied here) is resolved once a report of every
    /// survivor is in — never on part of them ("wait, never guess": a missing one blocks,
    /// and the coordinator's deadline rolls the world back): its drop set is made and the
    /// adoption read is wanted ([`Self::take_adoption_wanted`]).
    fn try_resolve_succession(&mut self, id: SuccessionId) {
        let me = self.ordinal();
        let Some(run) = self.successions.get(&id) else { return };
        if run.order.heir != me || !run.applied || self.adoptions.contains_key(&id) {
            return;
        }
        let Some(reports) = self.succession_reports.get(&id) else { return };
        let survivors: BTreeSet<HostId> = run.order.survivors.iter().copied().collect();
        if !survivors.iter().all(|h| reports.contains_key(h)) {
            return;
        }
        let other_base: Vec<HostId> =
            reports.iter().filter(|(_, r)| r.base_cut != run.order.base_cut).map(|(h, _)| *h).collect();
        if !other_base.is_empty() {
            // Never mixed with another cut's traffic: it waits (for ever: the deadline).
            log::error!(
                "[server] succession {}: the reports of hosts {other_base:?} count from another cut than {}: not resolved",
                id.0,
                run.order.base_cut.0
            );
            return;
        }
        let drop: BTreeSet<crate::EntityId> = reports
            .iter()
            .filter(|(h, _)| survivors.contains(h))
            .flat_map(|(_, r)| r.entries.iter())
            .filter_map(|e| match e.subject {
                Subject::Entity(eid) => Some(eid),
                Subject::Player(_) => None,
            })
            .collect();
        log::info!(
            "[server] succession {}: every survivor's report is in; {} entit(ies) to drop from host {}'s store, adoption read wanted",
            id.0,
            drop.len(),
            run.order.predecessor
        );
        let predecessor = run.order.predecessor;
        self.adoptions.insert(id, Adoption { predecessor, drop, state: AdoptionState::Wanted, pinned: BTreeSet::new() });
    }

    /// The adoption the server reads the store for, if any: `(succession, predecessor)`.
    /// It answers [`Self::adopted_rows`] with the predecessor's in-flight rows
    /// (`load_in_flight_of`) and the stored regions of [`Self::adoption_drop_set`]
    /// (`regions_of_entities`) — its fence already replaced by the claims it holds now — or
    /// [`Self::adoption_read_failed`].
    pub fn take_adoption_wanted(&mut self) -> Option<(SuccessionId, HostId)> {
        let (id, a) = self.adoptions.iter_mut().find(|(_, a)| a.state == AdoptionState::Wanted)?;
        a.state = AdoptionState::Asked;
        Some((*id, a.predecessor))
    }

    /// The drop set of succession `id`'s adoption: every entity a survivor's journal names
    /// (each lives elsewhere now, or is delivered from the journal) — none of them is taken
    /// from the predecessor's store.
    pub fn adoption_drop_set(&self, id: SuccessionId) -> Vec<crate::EntityId> {
        self.adoptions.get(&id).map(|a| a.drop.iter().copied().collect()).unwrap_or_default()
    }

    /// The server could not read what [`Self::take_adoption_wanted`] asked for: asked again.
    pub fn adoption_read_failed(&mut self, id: SuccessionId) {
        if let Some(a) = self.adoptions.get_mut(&id).filter(|a| a.state == AdoptionState::Asked) {
            // The server's reader warns (rate-limited); here only the state moves.
            log::debug!("[server] succession {}: the adoption read failed; asked again", id.0);
            a.state = AdoptionState::Wanted;
        }
    }

    /// The store's answer to [`Self::take_adoption_wanted`] for succession `id`: the
    /// predecessor's in-flight `rows` of the base cut and where the store holds each entity
    /// of the drop set (`located`). Steps 1–5 of the module docs, in the order they give.
    pub fn adopted_rows(&mut self, id: SuccessionId, mut rows: Vec<InFlightRow>, located: Vec<(crate::EntityId, RegionCoords)>) {
        let Some(adoption) = self.adoptions.get(&id).filter(|a| a.state == AdoptionState::Asked) else {
            log::warn!("[server] succession {}: adoption rows that were not asked for (or are answered); ignored", id.0);
            return;
        };
        let (pred, drop) = (adoption.predecessor, adoption.drop.clone());
        let Some(order) = self.successions.get(&id).map(|r| r.order.clone()) else {
            log::error!("[server] succession {}: adoption rows for a succession over here; ignored", id.0);
            return;
        };
        // The reports the drop set was made from (no report is filed after the resolution).
        let mut reports = self.succession_reports.remove(&id).unwrap_or_default();
        reports.retain(|h, _| order.survivors.contains(h));
        // 1. Resolve: per subject the latest event decides.
        let resolved = resolve(&reports);
        if order.kind == crate::SuccessionKind::Leave && resolved.received > 0 {
            log::error!(
                "[server] succession {} (a leave): {} entries say something came out of host {pred} after its cut; \
                 resolved like any other",
                id.0,
                resolved.received
            );
        }
        if resolved.ties > 0 {
            log::error!("[server] succession {}: {} subject(s) with two events at one stamp (a Received kept)", id.0, resolved.ties);
        }
        // 2. Adopt rows: what a survivor holds or is delivered from a journal is not taken.
        let all_rows = rows.len();
        rows.retain(|r| match r {
            InFlightRow::Transfer { bundle, .. } => !drop.contains(&bundle.id),
            InFlightRow::Intent { .. } => true,
        });
        // No region of the predecessor's starts from the store with a dropped entity:
        // installed before the mask is lifted (its flush may start one).
        let all_drop = self.adoption_drop_union();
        self.spawner.forget_entities(&all_drop);
        // What needs nothing of the land goes first: people reported absorbed (before any
        // region of it runs), players parked (a spawn order held while the land was masked
        // finds the journal's record once the mask lifts).
        let people = self.absorb_people(&resolved.people);
        let parked = self.park_players(id, &resolved.players);
        // A player that walked from the predecessor to another survivor: the store's record
        // of it (the base cut's) may stand in land this host owns now, and the predecessor's
        // pointer to that survivor died with it — this host stands in for it, before any
        // held spawn order is retried (a login would be placed a second time from the store).
        let pointed = self.point_at_holders(&resolved.held_by);
        // The land is this host's from here: what waited for it goes on.
        self.adopt_inherited(id);
        self.restore_in_flight(rows.clone());
        // 3. Deliver.
        self.deliver_bundles(&resolved.bundles);
        // 4. Load, pinned: every region the store holds a dropped entity in, and every one an
        // owed intent or reply goes to (a reply for a region that never ran here would have
        // nowhere to wait).
        let owed_targets = reports
            .values()
            .flat_map(|r| r.owed.iter())
            .flat_map(crate::intents_in)
            .map(|m| peers::peer_intent_target(&m));
        let pinned = self.load_pinned(located.iter().map(|(_, rc)| *rc).chain(owed_targets).collect());
        let owed = self.apply_owed(reports);
        self.succession_seam_events += resolved.entries as u64;
        log::info!(
            "[server] succession {}: adopted host {pred}'s land — {} journal entries resolved, {} bundle(s) delivered, \
             {parked} player(s) parked, {pointed} player(s) pointed at the survivor holding them, {people} person(s) \
             absorbed, {owed} owed intent(s)/repl(ies) applied, \
             {} of {all_rows} in-flight row(s) kept, {} region(s) loaded and pinned",
            id.0,
            resolved.entries,
            resolved.bundles.len(),
            rows.len(),
            pinned.len()
        );
        if let Some(a) = self.adoptions.get_mut(&id) {
            a.state = AdoptionState::Adopted;
            a.pinned = pinned;
        }
        // 5. Answered: the coordinator's next cut settles it (its part here adopts `pred`).
        self.world_send(HostMsg::SuccessionAdopted { id });
        // The spawn orders held since the heir applied go on now (unless another succession
        // of this host is still to adopt): a player parked above is found here.
        self.retry_held_spawns();
    }

    /// Step 3 for people: a person's bundle is never delivered. The receiving host reports
    /// the crossing, and this host is that host now — absorbed (no region of the land runs
    /// yet), so the history holds the person abstract and respawns them when it may. How
    /// many were reported.
    fn absorb_people(&mut self, people: &[(RegionCoords, crate::EntityBundle)]) -> usize {
        let mut reported = 0;
        for (target, bundle) in people {
            if let (EntityKind::Person { person }, true) = (bundle.kind, self.world.is_some()) {
                self.note_moved(person, *target, Step::Absorbed { at: bundle_at(bundle) });
                self.world_send(HostMsg::PersonCrossed { person, to: *target, delivered: false });
                reported += 1;
            }
        }
        reported
    }

    /// Step 3 for players: each parked here from its journalled record. How many.
    fn park_players(&mut self, id: SuccessionId, players: &[(HostId, crate::PlayerHandoff, PlayerRecord)]) -> usize {
        players.iter().cloned().filter(|(from, h, record)| self.park_from_journal(id, *from, h.clone(), record.clone())).count()
    }

    /// Step 3 for players another survivor holds (`held_by`: its `Received` entry is the
    /// player's latest event): this host — the owner of the land the predecessor's record of
    /// the player may stand in until the settle cut rewrites it — points at that survivor
    /// (`departed`), as the predecessor did, so a login of the player is sent on there and
    /// never placed here from the store's older record. Not for one this host holds itself.
    /// How many pointers were set.
    fn point_at_holders(&mut self, held_by: &[(PlayerId, HostId)]) -> usize {
        let me = self.ordinal();
        let mut pointed = 0;
        for &(player, holder) in held_by {
            if holder == me || self.holds_player(player) {
                continue;
            }
            self.departed.insert(player, holder);
            pointed += 1;
        }
        pointed
    }

    /// Step 3 for entities: each bundle arrives as any crossing from another host does
    /// (through `accept_arrival`: its region applies it at its next slot, never on receipt);
    /// a target another host owns gets it sent, one with no owner known keeps it waiting.
    fn deliver_bundles(&mut self, bundles: &[(RegionCoords, crate::Tick, crate::EntityBundle)]) {
        let now_ms = self.clock_ms;
        for (target, stamp, bundle) in bundles.iter().cloned() {
            if self.owns(target) {
                self.accept_arrival(None, target, stamp, bundle, now_ms);
            } else {
                let msg = Reliable::Transfer { target, stamp, bundle: Box::new(bundle), player: None };
                match self.foreign_host(target) {
                    Some(owner) => self.send_reliable(owner, msg, false),
                    None => self.wait_for_owner(target, msg, false, None),
                }
            }
        }
    }

    /// Step 4: every region of `regions` this host owns starts, and is pinned until its
    /// succession is over — also one whose load fails, or that is unavailable now: it is
    /// started again at its retry (`retry_unavailable`), and no cut part of this host is
    /// taken without it (`try_complete_cut`). The pinned regions.
    fn load_pinned(&mut self, regions: BTreeSet<RegionCoords>) -> BTreeSet<RegionCoords> {
        let mut pinned = BTreeSet::new();
        let owned: Vec<RegionCoords> = regions.into_iter().filter(|rc| self.owns(*rc)).collect();
        for rc in owned {
            self.succession_pinned.insert(rc);
            self.ensure_running(rc);
            pinned.insert(rc);
        }
        pinned
    }

    /// The `owed` intents and replies of every report, in each sender's order, as if that
    /// sender's session had brought them (accounted in no cut but this host's next). How
    /// many.
    fn apply_owed(&mut self, reports: BTreeMap<HostId, SuccessionReportIn>) -> usize {
        let mut owed = 0;
        for (from, report) in reports {
            for msg in report.owed {
                owed += 1;
                self.peer_reliable_in(from, msg, false);
            }
        }
        owed
    }

    /// A region this host pinned for an adoption that `held` says a pending cut part does
    /// not hold, if any: such a part is never taken — "wait, never guess".
    pub(super) fn pinned_missing(&self, held: impl Fn(&RegionCoords) -> bool) -> Option<RegionCoords> {
        self.succession_pinned.iter().copied().find(|rc| !held(rc))
    }

    /// A player survivor `from` handed (or moved) to the predecessor, its journal's latest:
    /// parked here from what was sent, at the hand-off's hop (never `departed`). No session
    /// comes for it — the client's home was the dead host, or still `from` while the hand-off
    /// was on the wire — so the client is told, as [`Self::park_handed`] tells it: a session
    /// of the client here is refused ([`PLAYER_PARKED_ELSEWHERE`], which a client rejoins on),
    /// `from` ends the one it may still hold ([`Reliable::PlayerParked`] — a notice, no state:
    /// not journalled, like R3's), and the login service hears that the login session is
    /// over. `false` if this host holds the player already (a login placed it meanwhile: that
    /// copy is the newer truth).
    fn park_from_journal(&mut self, id: SuccessionId, from: HostId, h: crate::PlayerHandoff, record: PlayerRecord) -> bool {
        if self.holds_player(h.player) {
            log::warn!("[server] succession {}: player {} is here already; the journal's copy is dropped", id.0, h.player.0);
            return false;
        }
        self.placed_by_handoff(h.player, h.hop);
        let hop = self.hop_here(h.player);
        self.world_send(HostMsg::PlayerOffline { player: h.player, hop });
        self.parked_players.insert(h.player, record);
        let here = self.sessions.get(&h.client).is_some_and(|s| s.player == Some(h.player));
        if here {
            log::info!("[server] succession {}: player {} (client {}) parked from the journal: its session here ends", id.0, h.player.0, h.client);
            let _ = self.out.send((Some(h.client), ServerPacket::Refused(PLAYER_PARKED_ELSEWHERE.to_string())));
            self.client_disconnected(h.client, self.clock_ms);
        }
        if from != self.ordinal() {
            log::info!("[server] succession {}: player {} (client {}) parked from host {from}'s journal: host {from} told", id.0, h.player.0, h.client);
            self.queue_to_peer(from, Reliable::PlayerParked { client: h.client, player: h.player });
        }
        self.login_parked_on_arrival(h.login);
        true
    }

    /// Whether this host is the heir of a succession it applied and has not adopted yet:
    /// every spawn order waits meanwhile (`placement.rs`), from the moment the mask goes up
    /// until the LAST such succession is adopted (or is over).
    pub(super) fn heir_not_adopted(&self) -> bool {
        let me = self.ordinal();
        self.successions.iter().any(|(id, r)| {
            r.applied && r.order.heir == me && self.adoptions.get(id).is_none_or(|a| a.state != AdoptionState::Adopted)
        })
    }

    /// Whether `rc` is land this host inherits and has not adopted yet (the mask).
    pub(super) fn inherited_not_adopted(&self, rc: RegionCoords) -> bool {
        self.inheriting.contains_key(&crate::super_region_of(rc, self.ownership.1))
    }

    /// Every drop set of an adoption not over yet.
    fn adoption_drop_union(&self) -> BTreeSet<crate::EntityId> {
        self.adoptions.values().flat_map(|a| a.drop.iter().copied()).collect()
    }

    /// The predecessors this host adopted and whose successions are not settled: its cut
    /// parts carry them ([`crate::WorldCut::adopts`]).
    pub(super) fn adopting_predecessors(&self) -> Vec<HostId> {
        let set: BTreeSet<HostId> =
            self.adoptions.values().filter(|a| a.state == AdoptionState::Adopted).map(|a| a.predecessor).collect();
        set.into_iter().collect()
    }

    /// Journal entries this host resolved as an heir, since it started (the `[server]` line).
    pub fn succession_seam_events(&self) -> u64 {
        self.succession_seam_events
    }

    /// The regions this host started for an adoption, pinned until its succession is over.
    pub fn succession_pinned(&self) -> Vec<RegionCoords> {
        self.succession_pinned.iter().copied().collect()
    }

    /// Whether host `ordinal`'s current boot was succeeded (until a hello of another boot):
    /// it holds nobody and is awaited by nothing.
    pub(super) fn peer_succeeded(&self, ordinal: HostId) -> bool {
        self.succeeded_peers.contains_key(&ordinal)
    }

    /// A hello of host `from`'s boot `boot` was accepted: if that ordinal was succeeded as
    /// another boot, it is a new host now.
    pub(super) fn succession_hello(&mut self, from: HostId, boot: u64) {
        if self.succeeded_peers.get(&from).is_some_and(|dead| *dead != boot) {
            log::info!("[server] host {from} is back as a new boot ({boot:#x}) after its succession");
            self.succeeded_peers.remove(&from);
        }
    }

    /// The successions this host answers (or waits to): `(id, applied)`.
    pub fn successions(&self) -> Vec<(SuccessionId, bool)> {
        self.successions.iter().map(|(id, r)| (*id, r.applied)).collect()
    }

    /// The heir's: the survivors' reports of succession `id` it holds, by survivor.
    pub fn succession_reports(&self, id: SuccessionId) -> Option<&BTreeMap<HostId, SuccessionReportIn>> {
        self.succession_reports.get(&id)
    }

    /// Every report held (the heir's), by succession and survivor.
    pub fn all_succession_reports(&self) -> &BTreeMap<SuccessionId, BTreeMap<HostId, SuccessionReportIn>> {
        &self.succession_reports
    }

    /// The heir adopted succession `id` (Task 9 calls this once resolved and loaded): the
    /// super-regions it inherits by it are its own from now on.
    /// It lifts the `inheriting` mask of those super-regions and delivers what waited for them
    /// (`flush_unowned`), then retries held spawn orders.
    pub fn adopt_inherited(&mut self, id: SuccessionId) {
        let before = self.inheriting.len();
        self.inheriting.retain(|_, s| *s != id);
        let lifted = before - self.inheriting.len();
        if lifted > 0 {
            log::info!("[server] succession {}: {lifted} inherited super-region(s) adopted", id.0);
        }
        self.flush_unowned();
        self.retry_held_spawns();
        // Subscribes that waited for the land are answered (served here, or the client told
        // to take a region here that it took from the predecessor).
        self.answer_asked_elsewhere();
    }

    /// The super-regions this host inherits and has not adopted yet, with their succession.
    pub fn inheriting(&self) -> &BTreeMap<crate::SuperRegion, SuccessionId> {
        &self.inheriting
    }

    /// Test support: whether this host's session with host `ordinal` is reachable now.
    #[doc(hidden)]
    pub fn peer_is_reachable(&self, ordinal: HostId) -> bool {
        self.peer_reachable(ordinal)
    }

    /// Test support: apply a coordinator message as if the world link had brought it.
    #[doc(hidden)]
    pub fn apply_coordinator_message(&mut self, msg: crate::CoordMsg) {
        let now = self.world_ms;
        self.apply_world_msg(msg, now);
        self.flush_world();
    }
}
