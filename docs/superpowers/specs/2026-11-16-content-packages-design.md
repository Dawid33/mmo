# Content packages and the package server — design

Date: 2026-11-16. Status: **draft**, not approved, nothing built. Written from Dawid's notes
(`docs/vault/Game Design.md`) and his answers to their review the same day.

Marks: **(D)** = Dawid decided it; **(call)** = Claude's proposal; **(open)** = undecided. The game-level
view is `docs/game-design.md` §7. How a quest from a package comes to life in a world is
`2026-11-16-quests-in-the-world-design.md`. The script language is
`2026-11-16-quest-script-language-design.md`.

## 1. What a package is

A **package** is a versioned, content-addressed bundle of authored content (D). It can hold:

| Content | Used by | When |
|---|---|---|
| Quest definitions: script, start requirements, type, lifetime | the coordinator's spawn checks, regions' script instances | after the forge |
| Dialogue trees, in a standard format | quest scripts | after the forge |
| Prefabs: blocks, entities, script instances, script blocks | site generators, quest scripts | at the forge, or placed later by a script |
| Worldgen code: site generators | the forge | at the forge, or later (§4) |
| Acts, and additions or edits to the world's quest line | the coordinator's story state | after the forge |
| Script modules | other packages' scripts (as `use`, quest-script spec §5.10) | at compile time |

The dialogue format is **(open)**; `2026-11-16-dialogue-and-quest-structure-research.md` recommends
dialogue and quest constructs native to the quest script language, modelled on Yarn Spinner 3 with ink's
weave, and a Yarn importer later. In that case a package ships compiled scripts plus a **string table per
locale**, keyed by line id.

## 2. Identity

- A package is named `<publisher>/<name>` (call). The publisher is an account on the package server; a
  name belongs to its publisher.
- Everything inside is addressed by a **qualified id**, `<publisher>/<name>::<item>`, so two packages
  never collide. A package that edits another's quest line names the other's ids, and must therefore
  depend on it.
- A package version is **semver**. A package **build** is identified by its **content hash** (SHA-256
  over a canonical archive: sorted paths, normalised metadata).

## 3. Versions, dependencies and locking (D)

- **While developing:** the manifest names dependencies by semver range. A resolver picks versions and
  writes a **lockfile**: each package's exact version and content hash.
- **Exported package:** pins every dependency **by hash**. Nothing a world runs is resolved again.
- **A world** holds one lock — the full, hash-pinned set of its enabled packages and their dependencies
  — in its world row (call), plus a history of each change (what, who, at which world tick).
- **One version of a package per world** (call): the resolver refuses a conflict. Two versions side by
  side would mean two sets of the same qualified ids.
- This is Nix's model without its lazy functional evaluation (D): declarative dependencies, fetched and
  cached by hash.

## 4. Enabling and disabling (D)

Packages are enabled at the forge or after it. Worldgen-affecting content can also be added and removed
after the forge. Making sure that does not break the world is the job of whoever does it. The engine's
side (call):

- **What changes on a mid-world change.** Only regions generated afterwards see a new site generator or
  prefab. A region with saved rows is never regenerated, and a parked region keeps what it saved. A
  region never saved generates under the *current* lock — "park, or regenerate" then gives different
  ground than an earlier lock would have. That is the admin's risk, and the tool warns about it
  whenever worldgen content changes.
- **Record which lock made a region.** Each region row records the hash of the lock it was generated
  under. Odd terrain can then be traced to a package change.
- **Disabling a package** reaps its live quest instances (quests-in-the-world §7). Its prefabs already
  in the world stay as ordinary blocks; nothing can run their scripts once the bytecode is gone. A quest
  whose requirements named its structures never starts again.
- **The forge itself is unchanged.** It is still a pure function of its inputs, and the lock is one of
  them.

## 5. Clients (D: clients need packages too)

- Online clients run the regions' script instances (they predict), so they need every script, module
  and dialogue the world's lock names. Single-player clients start their own server, so they need the
  whole package set, worldgen included.
- **Distribution (call).** The join answer carries the world's lock hash. A client lacking a build
  fetches it **by content hash** — from the world's own web Service (a copy beside the world, see
  below), falling back to the package server. It verifies the hash and caches it by hash; the browser
  uses the Cache API or IndexedDB in the worker. A build is immutable, so caching is forever and safe.
- **A world keeps copies of its locked builds** (call), in its own storage (`world-backups` or its
  database). A world never depends on the package server staying up, keeping a build, or not having
  yanked one, and a restore needs only the world's own backup.
- A lock change while clients are connected is announced as a stamped event. Clients fetch the new
  builds before the tick that first uses them, or are asked to rejoin **(open which)**.

## 6. Trust

- Package code runs only in the quest-script VM: fuel-bounded, deterministic, with no host access beyond
  the host API. That sandbox is the security boundary for the machines that run it.
- **Game balance is not guarded** (D): quests grant whatever their package says. Enabling a package is
  the trust decision, made by a world's admins.
- The package server checks structure on upload (the manifest parses, scripts compile against the host
  API version they declare, the hash matches). It does not judge content.

## 7. The package server

- **What it is (D):** its own deployment, for uploading, searching, browsing and downloading packages,
  and for enabling them for a world at the forge or after it.
- **Where it runs (D, with the topology rule):** on the **management** side, with its database in the
  **management PostgreSQL instance**, never in a world's cluster (`pg`). That instance is planned but
  not built: login, Keycloak and chat move to it after the elastic-hosts work. The package server is
  either its first tenant or waits for it **(open)**. Archives go in object storage — Garage, a
  `packages` bucket (call).
- **Its API (call):**
  - *Publish*: authenticated through Keycloak via the login service, as the publisher.
  - *Search and browse*: by name, publisher, kind of content, and text.
  - *Fetch by hash*: public and immutable, so it can sit behind a cache.
  - *Yank*: a version is hidden from resolution, but stays fetchable by hash, so existing locks keep
    working.
- **Enabling for a world** is not the package server's write. It is a world-side operation —
  `worldctl`, or the world panel — that resolves against the server, copies the builds into the world
  (§5) and updates the world's lock. The two never share a database.
- **Images and recipes:** a new `deploy/modules/package-server` and orion root, an ECR repository,
  `just` recipes as for `login`. Planned with the deployment, not here.

## 8. Tooling (call)

The `quest` CLI (its own workspace, `quest/`) grows the package commands. It must not pull in game
crates.

- `quest pkg new`, `quest pkg lock` (resolve and write the lockfile), `quest pkg build` (canonical
  archive and hash), `quest pkg check` (compile every script against the host API description), and
  `quest pkg publish`.
- A local package directory can be enabled for a dev world without publishing (`just` recipe), for the
  inner loop.

## 9. Open

- The dialogue format (§1).
- Publisher namespaces: personal accounts only, or organisations too.
- Licences and attribution on the server.
- Whether a package may *remove* another package's quest-line entries, or only add and edit them.
- Mid-session lock changes: hot fetch or rejoin (§5).
- Whether the package server waits for the management PostgreSQL instance (§7).
- Moderation of public packages — likely needed only once strangers publish.
