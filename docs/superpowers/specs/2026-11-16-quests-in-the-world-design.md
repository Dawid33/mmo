# Quests in the world — design

Date: 2026-11-16. Status: **draft**, not approved, nothing built. Written from Dawid's notes
(`docs/vault/Game Design.md`) and his answers to their review the same day.

Marks: **(D)** = Dawid decided it; **(call)** = Claude's proposal, to be confirmed or changed; **(open)** =
undecided. The game-level *what* and *why* are in `docs/game-design.md` §4–§7. This document is the
*how* of a quest's life in a running world. Packages, versions and the package server are in
`2026-11-16-content-packages-design.md`. The script language and its VM are in
`2026-11-16-quest-script-language-design.md`, and this document changes the in-game parts of that spec
(§2 below).

## 1. Terms

| Term | Meaning |
|---|---|
| **Quest definition** | Content from a package: the quest's script, its start requirements, its type (repeatable / one-off / world-line), its dialogue. Immutable once its package is locked. |
| **Quest instance** | One occurrence of a definition in a world, with an id, the bindings that met its requirements, and a state (§4). |
| **Spawn job** | The coordinator's durable row asking for one quest instance to be set up (§5). |
| **Script instance** | A running program: bytecode plus VM state (the quest-script VM's state, §6 of its spec). Region sim state — saved in the region's cut, hashed, undone on rollback, run on the server and on predicting clients alike. |
| **Script block** | A placed block that is an **access point** onto a region's script instances (D). It holds no code itself. |
| **Quest namespace** | The external key–value store of one quest instance (D: one per quest, no longer player-owned). |
| **Bindings** | What made the requirements true: a person, a site, a place, an entity. Passed to the script at setup. |

## 2. Changes to the quest-script spec

The standalone MVP is unaffected. These are its game-integration assumptions that no longer hold:

- **§8.1 Namespaces:** created, owned and deleted by **the system for one quest instance** (D), not by
  players. A namespace's blocks are that instance's script instances, in any number of regions. The
  coordinator remains its single writer; that rule stands.
- **"Per block" becomes "per script instance"** (call): one handler at a time, the event queue, parked
  handlers, reset, and state migration on edit (§6.9) all belong to the script instance. A region holds
  any number of script instances; a script block may show several.
- **Who edits:** admins in editor mode (D), through a script block. Reset and the edit-keeps-state rules
  of §6.9 apply unchanged to an admin's edit.
- Promise answers, event delivery and buffered host commands stay as specified.
- **Anchors and region access** (language spec §9.1): a script instance is anchored to a region or to an
  entity (it then crosses seams with that entity). It reads its region and the halo synchronously, writes
  through commands and `Intent`s, and reaches farther only by promise, all rollback-transparent.
- **Dialogue** is declarative data run by the NPC's region (language spec §5.11). Conversations work across
  seams: the conversation travels with the NPC, picks and `emit`s travel as stamped messages, and each
  attaching instance mirrors the state its conditions read to the NPC's region.

## 3. Requirements this design must keep

- The coordinator never writes region state, and a region never writes a table another host's part
  writes (CLAUDE.md, Server / store).
- Anything that enters the sim enters at one world tick on every machine, as a stamped event, never on
  receipt (the promise answers' rule).
- A host crash rolls a region back to its last committed cut, while the coordinator's committed rows
  survive. Every hand-off between the two must survive that (the review's "failure path", D).
- Nothing wakes a parked region for a quest. A quest is placed only into running regions near players.

## 4. A quest instance's life

```
definition ──(requirements met near players)──▶ Job: pending
   ──(delivered to the owning host, stamped into the region)──▶ Setting up
   ──(setup accepted, and a committed cut holds the script instance)──▶ Running
   ──(script reports an outcome)──▶ Completed ─▶ recorded; reset (repeatable) or closed
   ╰─(setup declines / times out)──▶ Failed ─▶ bindings cooled down
   ╰─(expiry, fault, package disabled, admin)──▶ Reaped
```

The instance's state is the coordinator's row. The script instances are region state. The two are
reconciled by the job id (§5).

## 5. Spawning: requirements, jobs and the failure path

### 5.1 Indexing definitions

When a world's package set changes, the coordinator reads every quest definition and stores its start
requirements in its database (D). It never re-parses packages per check.

### 5.2 Checking requirements (call)

**Periodically**, the coordinator checks the requirements of the definitions that could start (D),
restricted to **live areas**: regions with a player in them or within a few regions of one. "Players are
here" comes from the hosts — each host reports the regions holding players with its periodic messages —
because the coordinator does not track bodies.

Requirements are a **declarative query** (call), not arbitrary script code, so they can be indexed and
checked cheaply. Terms such as:

- *world facts* the coordinator holds: the current act and loop, completed quest instances, history and
  legends, sites and their kinds, settlement ranks and owners;
- *region facts* read from **region snapshots** (Dawid's suggestion): the region rows of the **last
  committed cut**, decoded off the sim by the coordinator (or a worker beside it) — entities of a kind
  or with a tag, block types and counts, fixtures, script instances already present;
- *distance and placement*: within N regions of live players, not within M regions of another instance
  of this definition.

Snapshot reads cost no host and no tick, and are consistent (one world tick). They are up to one save
interval stale, so a binding may no longer hold by setup time. The setup's decline path (§5.4) is the
answer, not a fresher read. A script-function escape hatch for requirements no query can express is
**(open)**: if it is added, it runs in a read-only VM with fuel, over the same snapshot.

### 5.3 Jobs

A check that passes inserts a **spawn job** row (D) in the same transaction that reserves its bindings
(a person or site bound to a job is not offered to another). Each job holds:

- the job id, the definition and its package hash;
- the target regions and the bindings;
- the job's state and a deadline.

The job id is the **idempotency key** for everything below. The world's per-region and per-world quest
budgets (§8) are checked in that transaction.

### 5.4 Placement — the failure path (call)

1. **Delivery.** The coordinator sends the job to the host owning each target region, as a new
   `CoordMsg`. The host stamps it into the region as an event at a world tick.
2. **Setup in the sim.** The region creates the script instance from the definition's bytecode,
   **unless one with this job id already exists** (a re-delivery). Its setup handler gets the bindings,
   and it ends with **accept** or **decline** (the binding is gone, the place is wrong).
3. **Answer.** The host reports accept or decline back, with the cut the region is in.
4. **Durable only in a committed cut.** The coordinator marks the instance *Running* only after a
   committed world cut contains that region's accepted script instance. A host that crashes before
   that cut loses the instance with its rollback, and the coordinator re-delivers. Re-delivery is
   harmless, because step 2 is idempotent on the job id.
5. **Decline or timeout.** The job fails, its bindings are released, and that definition with those
   bindings cools down (a named tunable) so the next check does not pick them straight back.
6. **Multi-region quests.** Every target region must accept. A decline in one, or the deadline, reaps
   the others' script instances (§7).

After *Running* the coordinator is **hands-off** (D). The script manages the quest. Its script instances
in different regions coordinate through the quest namespace, never through shared memory (the seam rule).

**Superseded for quest state (D, 2026-11-16, `2026-11-16-quest-construction-design.md` §3.6):** a quest's
stage, objectives and participants have one owner, the **home** instance. Its other instances are **parts**:
they read the home's state through a stamped mirror and change it only by `emit`. The namespace stays for
durable data (such as what survives a repeatable reset).

## 6. Completion

- The script reports the outcome through a host call — completed, failed or abandoned, with the
  participating players. It travels like any buffered host command and reaches the coordinator after
  the segment commits.
- The coordinator records it once, idempotent on the instance id:
  - **One-off** and **world-line**: closed for everyone, with a history / legends entry naming the
    participants. That entry is the player's renown (game-design §3.3).
  - **World-line**: additionally advances the act state (§9).
  - **Repeatable**: the instance resets per its definition, reusing the namespace (cleared or kept, as the
    definition says) and its script instances. It is recorded per (definition, player), with history entries
    only for a player's first completion and milestones (D, 2026-11-16; game-design §4.1).
- The script then removes itself from its region(s) (D) — the same removal as the reaper's (§7), started
  by the script.

## 7. Reaping

Everything a quest instance creates is **tagged with its instance id** (call): script instances, spawned
entities and people, placed blocks and fixtures, quest items. The reaper removes them all and closes the
namespace. It runs when:

- the script completes and removes itself (§6);
- a **lifetime** expires — the definition sets it, with a world default — counted only while no player
  is near, so a quest is not pulled from under a player;
- the script instance **faults** past a limit (a run of escaped errors in setup or its handlers);
- its **package is disabled** for the world (call);
- an **admin** reaps it in editor mode.

Reaping happens in the region, as a stamped event, so it rolls back and saves like any sim change. The
coordinator's instance row moves to *Reaped* only after a committed cut, as in §5.4.

What reaping does to a tagged item in a player's inventory — vanish, or turn into an ordinary item — is
**(open)**, and may be per definition.

## 8. Budgets (call)

Named tunables, checked when a job is created: live quest instances per region, per live area and per
world, and the cooldown of §5.4. Budgets keep a busy area from filling with quests, and keep a world's
quest rows bounded.

## 9. Acts and the world's story

- The coordinator holds the **story state** — current act, loop number, when the act opened — as world
  rows next to history. Act definitions come from packages: a set of world-line quests and the rule that
  completes the act.
- **Single player:** the next act opens when the act's quests are complete (D: quest-gated).
- **Multiplayer:** the next act opens when its quests are complete **and** the act's minimum duration has
  passed since it opened (D: time-gated, not time-started; this reading is Claude's — game-design §4.2).
- Requirements can name the act and the loop number, so a package can offer quests only in some acts,
  or differently on a later loop. What a later loop changes is **(open)**.
- **Single player or multiplayer** is a world setting chosen at the forge (call).

## 10. Editor mode and permissions (call)

- Per-world admins (D) are world data: a world-store table, granted and revoked by `worldctl`. They
  never live in the login service, which serves every world.
- An admin in editor mode, at a script block, lists that region's script instances and can view, edit,
  reset or reap them. Edits follow the quest-script spec's §6.9.
- An edit made in a world is a hot fix to that instance only. Keeping it means carrying it back into the
  package source; the editor can export the script for that.

## 11. Land claims and permissions (call — engine notes for game-design §5)

- A player's claim on a region is a coordinator row (single writer). In code it is a **holding**, because
  "claim" already means a host's super-regions (`game::ownership`).
- The sim enforces it per action (break, place, interact) from region-local state the coordinator pushes
  as a stamped event when the holding or its permission list changes. The check must not read
  coordinator state on the tick.

## 12. Open

- Requirement escape hatch: script functions over snapshots (§5.2).
- Tagged items after reaping (§7).
- What a non-admin sees at a script block.
- Whether a quest may target a parked region (here: never — §3).
- How snapshot decoding is shared with the hosts' codec without the coordinator linking the region sim
  (a `world-store` decode-only path is the likely answer).

## 13. Order of work (call)

1. The embark/return removal plan (game-design §10) — it touches the same coordinator code.
2. Script instances in the region sim: the VM behind the quest-script spec's host trait, saved in cuts,
   hashed and undone. One hand-placed instance, no jobs yet.
3. Quest namespaces on the coordinator (the promise path).
4. Spawn jobs with world-fact requirements only, the failure path, completion and reaping.
5. Region-snapshot requirements.
6. Acts.

Each step that adds sim state runs `just determinism-wasm`; anything crossing a seam runs `just test-threads`.
