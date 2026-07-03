# Undo API Phase 1 — Foundations Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the split undo log (global index + per-field closure queues) with a single transaction-tagged log holding typed deltas or opaque closures, add `UndoCell`/`UndoMap` tier-1 wrappers, and port the scalar/map fields of `GameData` plus their `region.rs` call sites.

**Architecture:** All changes ride on the existing `#[rollback]` proc macro in `crates/macros/src/lib.rs` — it keeps generating the module contents, but emits the new log model. Tier-2 closures (`undo()`) keep working unchanged through the whole plan; tier-1 wrappers are introduced per field via a new `#[undo(...)]` field attribute. Every task leaves the workspace green (build + tests + the game runs).

**Tech Stack:** Rust (stable 1.96 must build), syn/quote proc macro, crc32fast hashing, crossbeam channels.

## Global Constraints

- `cargo build --workspace --bins` must pass on stable at every commit.
- NEVER run bare `cargo test -p rollback` — the pre-existing `tests/simple.rs` and `tests/deep.rs` don't compile (known, out of scope). Always name tests: `cargo test -p rollback --test rollback_restore --test hash_restore --test log_model`.
- No new external dependencies.
- Hash-verification semantics are inviolable: every log entry records the crc32 of the field's state *before* the mutation; `rollback()` asserts the state hashes to that value *after* the undo is applied.
- Determinism: no `HashMap` iteration in anything that touches logged state; keep `BTreeMap`.
- The end-to-end smoke test is: server + client run ≥15 s with zero panics and the client logs `Region recieved and loaded!` (see Task 6).
- Phases 2–5 of the spec (slotmapd fork, auto-emit, rapier fork, cleanup) are **out of scope** — they get their own plans.

---

### Task 1: Baseline regression tests for behavior that must survive the refactor

**Files:**
- Create: `crates/rollback/tests/log_model.rs`

**Interfaces:**
- Consumes: current public API — `Rollback::new(Option<Sender<GameDataUpdate>>)`, `new_transaction()`, `rollback()`, `forget()`, `current()`, `oldest()`, field access via `Deref`, `Undo::undo()`, `create_player_safe`, `create_mesh`.
- Produces: the invariant suite Tasks 2–5 must keep green. No new library code.

- [ ] **Step 1: Write the tests**

```rust
//! Invariants of the transaction log that must hold before and after the
//! log-model refactor. Uses only public API that exists in both worlds.
use std::hash::Hash;

use rollback::{ChunkCoords, Rollback};

fn state_hash(r: &Rollback) -> u32 {
    let mut hasher = crc32fast::Hasher::new();
    r.hash(&mut hasher);
    hasher.finalize()
}

fn new_rollback() -> (Rollback, crossbeam::channel::Receiver<rollback::GameDataUpdate>) {
    let (send, recv) = crossbeam::channel::unbounded();
    (Rollback::new(Some(send)), recv)
}

#[test]
fn multi_transaction_rollback_restores_each_boundary() {
    let (mut r, _recv) = new_rollback();
    let h0 = state_hash(&r);

    r.new_transaction();
    r.create_mesh(ChunkCoords::new(0, 0, 0));
    let h1 = state_hash(&r);

    r.new_transaction();
    r.create_player_safe(7);
    assert_ne!(h1, state_hash(&r));

    r.rollback();
    assert_eq!(h1, state_hash(&r), "rollback of tx2 must land on tx1 boundary");
    r.rollback();
    assert_eq!(h0, state_hash(&r), "rollback of tx1 must land on initial state");
}

#[test]
fn forget_drops_oldest_transaction_and_keeps_state() {
    let (mut r, _recv) = new_rollback();

    r.new_transaction();
    r.create_mesh(ChunkCoords::new(0, 0, 0));
    r.new_transaction();
    r.create_player_safe(7);
    let h2 = state_hash(&r);

    r.forget(); // drop tx1's undo info; state untouched
    assert_eq!(h2, state_hash(&r));
    assert_eq!(r.oldest(), 1);

    // tx2 must still be rollback-able after forgetting tx1.
    r.rollback();
    // No hash target for the tx1 boundary anymore, but rollback must not
    // panic and must decrement current.
    assert_eq!(r.current(), 1);
}

#[test]
fn lifo_order_is_preserved_across_fields() {
    // player_entites and tick are different fields; undos must pop LIFO
    // across fields within a transaction. Uses tier-2 undo() directly.
    let (mut r, _recv) = new_rollback();
    let h0 = state_hash(&r);

    r.new_transaction();
    r.tick.undo(|d, _| *d -= 1);
    *r.tick += 1;
    r.player_entites.undo(|d, _| {
        d.remove(&3);
    });
    r.player_entites.insert(3, rollback::EntityKey::default());
    r.tick.undo(|d, _| *d -= 10);
    *r.tick += 10;

    r.rollback();
    assert_eq!(h0, state_hash(&r));
}
```

- [ ] **Step 2: Run tests, expect PASS (they encode current behavior)**

Run: `cargo test -p rollback --test log_model`
Expected: 3 passed. If `lifo_order_is_preserved_across_fields` fails to compile because `tick`/`player_entites` mutation needs `DerefMut` — that access exists today (`region.rs` uses `*self.data.tick += 1`); fix the test, not the library.

- [ ] **Step 3: Commit**

```bash
git add crates/rollback/tests/log_model.rs
git commit -m "test: pin transaction-log invariants before log-model refactor"
```

---

### Task 2: Unify the log — single queue of `Entry { transaction, undo: FieldUndo, pre_hash }`

**Files:**
- Modify: `crates/macros/src/lib.rs` (items "RollbackLog", "struct Undo<T>", "impl Undo<T>", "impl DelayedUndo<T>", "impl Rollback" rollback/forget, "impl new for Rollback", "impl Rollback reinitialize")
- Test: existing `crates/rollback/tests/{log_model,rollback_restore,hash_restore}.rs` (no new tests — this task is behavior-preserving)

**Interfaces:**
- Consumes: the macro's existing per-field iterators: `iter_log_ident` (flattened unique idents like `entities1`), `iter_path` (field access paths like `ecs.entities`), `iter_ty` (field types), `iter_path_string`.
- Produces (macro-generated, inside the `game_data` module):
  - `pub enum FieldUndo { root(Box<dyn FnOnce(&mut GameData, &Sender<GameDataUpdate>) + Send>), <log_ident>(Box<dyn FnOnce(&mut <ty>, &Sender<GameDataUpdate>) + Send>), ... }`
  - `pub struct Entry { pub transaction: usize, pub undo: FieldUndo, pub pre_hash: u32 }`
  - `RollbackLog { pub log: VecDeque<Entry>, pub client: Option<Sender<GameDataUpdate>>, info: RollbackInfo }` — per-field queues deleted.
  - `Undo<T>` gains `wrap: Option<fn(Box<dyn FnOnce(&mut T, &Sender<GameDataUpdate>) + Send>) -> FieldUndo>` and loses `log` and `field`.
  - Task 3 extends `Entry.undo` to an `UndoOp` enum; Task 5 builds `UndoScope` on `wrap`.

- [ ] **Step 1: Replace the `RollbackLog` item**

In `boilerplate`d item `"RollbackLog"` (currently `macros/src/lib.rs:381-392`), generate:

```rust
// enum FieldUndo — one variant per field path, plus the root struct.
// Tuple-variant constructors double as the `wrap` fn pointers.
#[allow(non_camel_case_types)]
pub enum FieldUndo {
    root(Box<dyn FnOnce(&mut #root_struct_ident, &::crossbeam::channel::Sender<crate::GameDataUpdate>) + Send>),
    #(#iter_log_ident1(Box<dyn FnOnce(&mut #iter_ty1, &::crossbeam::channel::Sender<crate::GameDataUpdate>) + Send>),)*
}

pub struct Entry {
    pub transaction: usize,
    pub undo: FieldUndo,
    pub pre_hash: u32,
}

#[derive(::core::default::Default)]
pub struct RollbackLog {
    pub log: ::std::collections::VecDeque<Entry>,
    pub client: ::std::option::Option<::crossbeam::channel::Sender<crate::GameDataUpdate>>,
    info: RollbackInfo,
}
```

- [ ] **Step 2: Rework `Undo<T>`**

In item `"struct Undo<T>"`: delete the `log` and `field` fields, add `wrap`:

```rust
#[serde(skip)]
#[debug(skip)]
wrap: ::std::option::Option<fn(Box<dyn FnOnce(&mut T, &::crossbeam::channel::Sender<crate::GameDataUpdate>) + Send>) -> FieldUndo>,
```

In item `"impl Undo<T>"`, `undo()` becomes (hash BEFORE pushing, as today):

```rust
pub fn undo(&mut self, undo: impl FnOnce(&mut T, &::crossbeam::channel::Sender<crate::GameDataUpdate>) + 'static + Send) {
    let mut global = self.global_log.lock().unwrap();
    let trans = self.info.current.load(::std::sync::atomic::Ordering::SeqCst);
    let pre_hash = unsafe { self.hash_data() };
    let wrap = self.wrap.expect("Undo field not wired to a FieldUndo variant");
    global.log.push_back(Entry { transaction: trans, undo: wrap(Box::new(undo)), pre_hash });
}
```

`DelayedUndo::undo` (item `"impl DelayedUndo<T>"`) identically, but with `pre_hash: self.hash` (the value captured at `delayed_undo()` time).

- [ ] **Step 3: Rework `rollback()` and `forget()`**

In item `"impl Rollback"`:

```rust
pub fn rollback(&mut self) {
    let rollback_log = self.log.clone();
    let mut rollback_log = rollback_log.lock().unwrap();
    let current = rollback_log.info.current.load(::std::sync::atomic::Ordering::SeqCst);
    while let Some(entry) = rollback_log.log.pop_back() {
        if entry.transaction != current {
            rollback_log.log.push_back(entry);
            break;
        }
        match entry.undo {
            FieldUndo::root(f) => {
                f(&mut self.data.data, rollback_log.client.as_ref().unwrap());
                let new_hash = unsafe { self.data.hash_data() };
                if new_hash != entry.pre_hash {
                    panic!("Hash verification failed for root in transaction {:?}: {:?} != {:?}", entry.transaction, new_hash, entry.pre_hash);
                }
            }
            #(FieldUndo::#iter_log_ident1(f) => {
                let previous_data = self.#iter_path5.data.clone();
                f(&mut self.#iter_path1.data, rollback_log.client.as_ref().unwrap());
                let new_hash = unsafe { self.#iter_path6.hash_data() };
                if new_hash != entry.pre_hash {
                    println!("Hash verification failed for self.{} in transaction {:?}: {:?} != {:?}\nlog len: {:?}", #iter_path_string1, entry.transaction, new_hash, entry.pre_hash, rollback_log.log.len());
                    match ::assert_json_diff::assert_json_matches_no_panic(&self.#iter_path7.data, &previous_data, ::assert_json_diff::Config::new(::assert_json_diff::CompareMode::Strict)) {
                        Ok(()) => panic!("Before and after is equal via serde_json"),
                        Err(e) => panic!("lhs: new, rhs: old. {}", e),
                    }
                }
            })*
        }
    }
    rollback_log.info.current.store(current - 1, ::std::sync::atomic::Ordering::SeqCst);
}

pub fn forget(&mut self) {
    let rollback_log = self.log.clone();
    let mut rollback_log = rollback_log.lock().unwrap();
    let oldest = rollback_log.info.oldest.load(::std::sync::atomic::Ordering::SeqCst);
    let current = rollback_log.info.current.load(::std::sync::atomic::Ordering::SeqCst);
    if oldest >= current {
        panic!("Cannot forget transaction that doesn't exist. oldest, current = {:?}, {:?}", oldest, current);
    }
    while let Some(entry) = rollback_log.log.pop_front() {
        if oldest + 1 < entry.transaction {
            rollback_log.log.push_front(entry);
            break;
        }
        // Entries are data; dropping them is the whole job now.
    }
    rollback_log.info.oldest.store(oldest + 1, ::std::sync::atomic::Ordering::SeqCst);
}
```

- [ ] **Step 4: Rework wiring in `new()` / `reinitialize()`**

Replace the per-field queue/`field`-index wiring (`r.#iter_path1.log = ...`, `r.#iter_path4.field = ...`) with variant-constructor wiring (both items, same lines):

```rust
r.data.wrap = Some(FieldUndo::root);
#(r.#iter_path2.global_log = log.clone();)*
#(r.#iter_path4.wrap = Some(FieldUndo::#iter_log_ident1);)*
let mut log = log.lock().unwrap();
#(r.#iter_path3.info = log.info.clone();)*
```

- [ ] **Step 5: Build and run the invariant suite**

Run: `cargo build -p rollback && cargo test -p rollback --test log_model --test rollback_restore --test hash_restore`
Expected: builds clean; 3 + 2 + 3 tests PASS. Then `cargo build --workspace --bins` — `game`, `client`, `server` must still compile (they only use `undo`/`delayed_undo`/`change`/Deref, all preserved).

- [ ] **Step 6: Commit**

```bash
git add crates/macros/src/lib.rs
git commit -m "refactor: unify rollback log into single Entry queue with FieldUndo dispatch"
```

---

### Task 3: Typed deltas + `UndoCell`, port `tick` and `next_game_event_id`

**Files:**
- Modify: `crates/macros/src/lib.rs` (field-attribute parsing; new items `UndoOp`, `Delta`, `UndoCell`; wiring; rollback match arms)
- Modify: `crates/rollback/src/rollback.rs:295-302` (annotate `GameData` fields)
- Modify: `crates/game/src/region.rs:143-146,181-182` (call sites)
- Test: `crates/rollback/tests/log_model.rs`

**Interfaces:**
- Consumes: `Entry`/`FieldUndo`/wiring from Task 2.
- Produces:
  - Field attribute `#[undo(cell)]` on module struct fields → field type becomes `UndoCell<T>`.
  - `pub struct UndoCell<T> { data: T, global_log, info, make: Option<fn(T) -> Delta> }` with `pub fn set(&mut self, v: T)`, `pub fn update(&mut self, f: impl FnOnce(&mut T))`, `Deref` (no `DerefMut`), `Hash`, serde/Clone like `Undo<T>`.
  - `#[allow(non_camel_case_types)] pub enum Delta { <log_ident>(T), ... }` — tuple variant per `#[undo(cell)]` field holding the OLD value.
  - `Entry.undo` type changes to `pub enum UndoOp { Opaque(FieldUndo), Typed(Delta) }`.
  - Task 4 adds map variants to `Delta`; Task 6 relies on `update()` at call sites.

- [ ] **Step 1: Write failing tests**

Append to `crates/rollback/tests/log_model.rs`:

```rust
#[test]
fn undocell_update_is_rolled_back() {
    let (mut r, _recv) = new_rollback();
    let h0 = state_hash(&r);
    r.new_transaction();
    r.tick.update(|t| *t += 1);
    r.next_game_event_id.update(|n| *n += 5);
    assert_eq!(*r.tick, 1);
    r.rollback();
    assert_eq!(*r.tick, 0);
    assert_eq!(h0, state_hash(&r));
}

#[test]
fn undocell_set_is_rolled_back() {
    let (mut r, _recv) = new_rollback();
    let h0 = state_hash(&r);
    r.new_transaction();
    r.tick.set(42);
    assert_eq!(*r.tick, 42);
    r.rollback();
    assert_eq!(h0, state_hash(&r));
}
```

Also update `lifo_order_is_preserved_across_fields`: replace `r.tick.undo(...); *r.tick += 1;` pairs with `r.tick.update(|t| *t += 1);` / `r.tick.update(|t| *t += 10);` (tick stops being `Undo<usize>`).

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p rollback --test log_model`
Expected: compile FAIL — no `update` on `Undo<usize>`.

- [ ] **Step 3: Implement in the macro**

3a. Parse field attributes. In the `for mut i in items.iter_mut()` loop that wraps field types, first read and strip `#[undo(...)]`:

```rust
fn undo_kind(f: &syn::Field) -> Option<syn::Ident> {
    f.attrs.iter().find_map(|a| {
        if !a.path().is_ident("undo") { return None; }
        a.parse_args::<syn::Ident>().ok()
    })
}
// in the loop, per field:
let kind = undo_kind(f);
f.attrs.retain(|a| !a.path().is_ident("undo"));
let ty = &f.ty;
f.ty = match kind.as_ref().map(|k| k.to_string()).as_deref() {
    Some("cell") => syn::Type::parse.parse2(quote! { UndoCell<#ty> }).unwrap(),
    None => syn::Type::parse.parse2(quote! { Undo<#ty> }).unwrap(),
    Some(other) => panic!("unknown undo kind {other}"),
};
```

Record the kind alongside each entry in `paths` (change `paths` to `Vec<(TokenStream, syn::Field, Option<String>)>`) — the path traversal must capture kinds from the ORIGINAL fields (run `undo_kind` during traversal, before the mutation loop strips attrs).

3b. New items:

```rust
pub enum UndoOp {
    Opaque(FieldUndo),
    Typed(Delta),
}
// Entry.undo: UndoOp   (update Entry definition from Task 2)

#[allow(non_camel_case_types)]
pub enum Delta {
    #(#cell_log_ident(#cell_inner_ty),)*   // old value
}

#[derive(::core::default::Default, ::derive_more::Debug, ::serde::Serialize, ::serde::Deserialize, ::std::clone::Clone)]
pub struct UndoCell<T>
where T: ::core::default::Default + ::std::clone::Clone + ::serde::Serialize + ::std::marker::Send + 'static + ::std::hash::Hash {
    #[serde(skip)] #[debug(skip)]
    global_log: ::std::sync::Arc<::std::sync::Mutex<RollbackLog>>,
    #[serde(skip)] #[debug(skip)]
    info: RollbackInfo,
    #[serde(skip)] #[debug(skip)]
    make: ::std::option::Option<fn(T) -> Delta>,
    #[debug(skip)]
    data: T,
}

impl<T> UndoCell<T> where T: /* same bounds */ {
    fn log_old(&mut self) {
        let mut global = self.global_log.lock().unwrap();
        let trans = self.info.current.load(::std::sync::atomic::Ordering::SeqCst);
        let mut hasher = ::crc32fast::Hasher::new();
        ::std::hash::Hash::hash(&self.data, &mut hasher);
        let pre_hash = hasher.finalize();
        let make = self.make.expect("UndoCell not wired");
        global.log.push_back(Entry { transaction: trans, undo: UndoOp::Typed(make(self.data.clone())), pre_hash });
    }
    pub fn set(&mut self, v: T) { self.log_old(); self.data = v; }
    pub fn update(&mut self, f: impl FnOnce(&mut T)) { self.log_old(); f(&mut self.data); }
}
// Deref (no DerefMut) + Hash impls identical in shape to Undo<T>'s.
```

Where `cell_log_ident`/`cell_inner_ty` iterate only `paths` entries whose kind is `Some("cell")`. The cell fields' `Undo`-specific wiring (`wrap`) is replaced by `r.#path.make = Some(Delta::#cell_log_ident);` in `new()`/`reinitialize()`; `global_log`/`info` wiring stays the same for all wrapper kinds.

3c. Existing `undo()` push sites wrap in `UndoOp::Opaque(...)`. `rollback()` match becomes `match entry.undo { UndoOp::Opaque(FieldUndo::...) => ..., UndoOp::Typed(d) => match d { ... } }` with a generated arm per cell field:

```rust
#(Delta::#cell_log_ident(old) => {
    self.#cell_path.data = old;
    let mut hasher = ::crc32fast::Hasher::new();
    ::std::hash::Hash::hash(&self.#cell_path.data, &mut hasher);
    let new_hash = hasher.finalize();
    if new_hash != entry.pre_hash {
        panic!("Hash verification failed for typed undo of self.{}: {:?} != {:?}", #cell_path_string, new_hash, entry.pre_hash);
    }
})*
```

3d. Annotate the fields in `crates/rollback/src/rollback.rs`:

```rust
pub struct GameData {
    ecs: Ecs,
    physics: PhysicsState,
    #[undo(cell)]
    tick: usize,
    #[undo(cell)]
    next_game_event_id: usize,
    player_entites: BTreeMap<ClientId, EntityKey>,
    clients: BTreeMap<ClientId, Client>,
}
```

3e. Port call sites in `crates/game/src/region.rs`:

```rust
// handle_event, lines 143-146: next_game_event_id
let event = GameEvent::new(event, *self.data.next_game_event_id, self.id);
self.data.new_transaction();
self.data.next_game_event_id.update(|n| *n += 1);
// Tick arm, lines 181-182:
self.data.tick.update(|t| *t += 1);
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p rollback --test log_model --test rollback_restore --test hash_restore && cargo build --workspace --bins`
Expected: all PASS, workspace builds.

- [ ] **Step 5: Commit**

```bash
git add crates/macros/src/lib.rs crates/rollback/src/rollback.rs crates/game/src/region.rs crates/rollback/tests/log_model.rs
git commit -m "feat: typed Delta log entries + UndoCell; port tick and next_game_event_id"
```

---

### Task 4: `UndoMap` for BTreeMap fields, port `player_entites` and `clients`

**Files:**
- Modify: `crates/macros/src/lib.rs` (`#[undo(map)]` kind, `UndoMap`, `Delta` map variants, rollback arms, wiring)
- Modify: `crates/rollback/src/rollback.rs` (`#[undo(map)]` on the two fields; `create_player_safe` tail)
- Modify: `crates/game/src/region.rs` (Tick / PlayerWinitEvent / CreateClient arms)
- Test: `crates/rollback/tests/log_model.rs`

**Interfaces:**
- Consumes: `UndoOp`/`Delta`/wiring pattern from Task 3.
- Produces:
  - `#[undo(map)]` on `BTreeMap<K, V>` fields → field type `UndoMap<K, V>`.
  - `pub struct UndoMap<K, V> { data: BTreeMap<K, V>, global_log, info, make: Option<fn(K, Option<V>) -> Delta> }`
  - Methods: `insert(&mut self, k: K, v: V) -> Option<V>`, `remove(&mut self, k: &K) -> Option<V>`, `get_mut(&mut self, k: &K) -> Option<&mut V>` (all log first), `get`, `keys`, `iter`, `len` (read-only, unlogged), `Deref<Target = BTreeMap<K, V>>` for reads. No `DerefMut`.
  - `Delta` map variants: `#(#map_log_ident(#K, Option<#V>),)*` — the key plus the PRIOR value at that key (`None` = key was absent). Revert: `Some(v) => map.insert(k, v)`, `None => map.remove(&k)`.
  - K, V extracted from the field's `BTreeMap<K, V>` type via `syn::PathArguments::AngleBracketed`; K bounds: `Ord + Clone + Send + 'static`, V bounds: same as cell T.

- [ ] **Step 1: Write failing tests**

Append to `log_model.rs`:

```rust
#[test]
fn undomap_ops_roll_back_exactly() {
    let (mut r, _recv) = new_rollback();
    r.new_transaction();
    r.player_entites.insert(1, rollback::EntityKey::default());
    let h1 = state_hash(&r);

    r.new_transaction();
    r.player_entites.insert(1, rollback::EntityKey::default()); // overwrite
    r.player_entites.insert(2, rollback::EntityKey::default()); // fresh insert
    r.player_entites.remove(&1);                                 // remove
    r.rollback();
    assert_eq!(h1, state_hash(&r), "insert-overwrite/insert/remove must all revert");
    assert!(r.player_entites.get(&1).is_some());
    assert!(r.player_entites.get(&2).is_none());
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p rollback --test log_model`
Expected: compile FAIL — `insert` not found / `player_entites` is `Undo<BTreeMap<..>>`.

- [ ] **Step 3: Implement**

3a. Macro: `Some("map")` kind → extract `(K, V)` from the `BTreeMap<K, V>` path; field type `UndoMap<#K, #V>`; `Delta` gains `#map_log_ident(#K, Option<#V>)`; wiring `r.#path.make = Some(Delta::#map_log_ident);`.

```rust
pub struct UndoMap<K, V> /* bounds per Interfaces */ {
    #[serde(skip)] #[debug(skip)]
    global_log: ::std::sync::Arc<::std::sync::Mutex<RollbackLog>>,
    #[serde(skip)] #[debug(skip)]
    info: RollbackInfo,
    #[serde(skip)] #[debug(skip)]
    make: ::std::option::Option<fn(K, ::std::option::Option<V>) -> Delta>,
    #[debug(skip)]
    data: ::std::collections::BTreeMap<K, V>,
}

impl<K, V> UndoMap<K, V> /* bounds */ {
    fn log_entry(&mut self, key: K, prev: ::std::option::Option<V>) {
        let mut global = self.global_log.lock().unwrap();
        let trans = self.info.current.load(::std::sync::atomic::Ordering::SeqCst);
        let mut hasher = ::crc32fast::Hasher::new();
        ::std::hash::Hash::hash(&self.data, &mut hasher);
        let pre_hash = hasher.finalize();
        let make = self.make.expect("UndoMap not wired");
        global.log.push_back(Entry { transaction: trans, undo: UndoOp::Typed(make(key, prev)), pre_hash });
    }
    pub fn insert(&mut self, k: K, v: V) -> ::std::option::Option<V> {
        self.log_entry(k.clone(), self.data.get(&k).cloned());
        self.data.insert(k, v)
    }
    pub fn remove(&mut self, k: &K) -> ::std::option::Option<V> {
        if let Some(prev) = self.data.get(k).cloned() {
            self.log_entry(k.clone(), Some(prev));
        }
        self.data.remove(k)
    }
    pub fn get_mut(&mut self, k: &K) -> ::std::option::Option<&mut V> {
        if let Some(prev) = self.data.get(k).cloned() {
            self.log_entry(k.clone(), Some(prev));
        }
        self.data.get_mut(k)
    }
}
```

Rollback arm per map field:

```rust
#(Delta::#map_log_ident(key, prev) => {
    match prev {
        Some(v) => { self.#map_path.data.insert(key, v); }
        None => { self.#map_path.data.remove(&key); }
    }
    /* hash verify identical to cell arm */
})*
```

3b. `crates/rollback/src/rollback.rs`: annotate `player_entites` and `clients` with `#[undo(map)]`. `create_player_safe` tail becomes:

```rust
self.data.player_entites.insert(client_id, e);
```

(the manual `undo` registration from commit `5111aae` is deleted — `UndoMap::insert` logs it).

3c. `crates/game/src/region.rs` — the three arms:

```rust
GameEventKind::CreateClient(client_id) => {
    info!("{:?}", event);
    self.data.clients.insert(client_id, Client::default());
    self.data.create_player_safe(client_id);
}
```

Tick arm — the manual `clients.undo`/`delayed_undo` input machinery collapses; `get_mut` logs the whole prior `Client` (input state is small):

```rust
GameEventKind::Tick => {
    for c in self.controllers.iter_mut() {
        let data = self.data.as_refs_mut();
        c.on_tick(data.data);
    }
    let data: &mut GameData = self.data.deref_mut();
    let keys: Vec<ClientId> = data.clients.keys().cloned().collect();
    for client_id in keys {
        let toggled = {
            let client = data.clients.get_mut(&client_id).unwrap();
            let toggle = client.input.key_pressed(&winit::keyboard::KeyCode::KeyE);
            if toggle { client.fps_cam_mode = !client.fps_cam_mode; }
            let _ = client.input.step(); // undo funcs unused: get_mut logged the clone
            toggle.then_some(client.fps_cam_mode)
        };
        if let Some(mode) = toggled {
            data.ecs.send(GameDataUpdate::new(
                crate::GameDataTransactionKind::Do,
                crate::GameDataUpdateKind::SetFreeCam(client_id, mode),
            ));
        }
    }
    self.data.tick.update(|t| *t += 1);
}
```

PlayerWinitEvent arm:

```rust
GameEventKind::PlayerWinitEvent(client_id, player_event) => {
    let data: &mut GameData = self.data.deref_mut();
    if let Some(c) = data.clients.get_mut(&client_id) {
        let _ = c.input.update(player_event.clone());
    }
}
```

Note: `WinitInput::step`/`update` still return undo closures; the returns are discarded here. Removing those return values is Phase 5 cleanup — do NOT change `input.rs` in this task.

- [ ] **Step 4: Run tests + build**

Run: `cargo test -p rollback --test log_model --test rollback_restore --test hash_restore && cargo build --workspace --bins`
Expected: all PASS. `lifo_order_is_preserved_across_fields` needs its `player_entites.undo(...)` pair replaced by plain `r.player_entites.insert(3, ...)` — update it in this task.

- [ ] **Step 5: Commit**

```bash
git add crates/macros/src/lib.rs crates/rollback/src/rollback.rs crates/game/src/region.rs crates/rollback/tests/log_model.rs
git commit -m "feat: UndoMap with per-entry deltas; port player_entites and clients"
```

---

### Task 5: `undo_scope()` guard replaces `delayed_undo()`

**Files:**
- Modify: `crates/macros/src/lib.rs` (replace `DelayedUndo` items with `UndoScope`)
- Modify: `crates/rollback/src/rollback.rs` (`create_entity_safe` migrates)
- Test: `crates/rollback/tests/log_model.rs`

**Interfaces:**
- Consumes: `Entry`/`UndoOp::Opaque`/`wrap` from Tasks 2–3.
- Produces:
  - `Undo<T>::undo_scope(&mut self) -> UndoScope<'_, T>` — captures `pre_hash` at creation.
  - `UndoScope`: `Deref`/`DerefMut` to `T` (deref_mut sets an internal `touched` flag), `pub fn register(mut self, f: impl FnOnce(&mut T, &Sender<GameDataUpdate>) + 'static + Send)` pushes `Entry { transaction, UndoOp::Opaque(wrap(f)), pre_hash }`.
  - `Drop`: `debug_assert!(!self.touched || self.registered, "UndoScope mutated without register()")`.
  - `delayed_undo()`/`DelayedUndo` are deleted (all callers migrate in this task).

- [ ] **Step 1: Write failing tests**

Slotmap true inverses arrive in Phase 2, so these tests exercise `UndoScope`
with snapshot semantics on the `ecs` field (exactly how `create_entity_safe`
uses it after this task). `scope.entities.deref_mut()` resolves through
`UndoScope: DerefMut → Ecs → .entities: Undo<SlotMap> → deref_mut()`.

```rust
use std::ops::DerefMut;

#[test]
fn undo_scope_snapshot_registration_rolls_back() {
    let (mut r, _recv) = new_rollback();
    let h0 = state_hash(&r);
    r.new_transaction();
    let old: rollback::Ecs = (*r.ecs).clone();
    let mut scope = r.ecs.undo_scope();
    let _key = scope.entities.deref_mut().insert(());
    scope.register(move |ecs, _| *ecs = old);
    r.rollback();
    assert_eq!(h0, state_hash(&r));
}

#[test]
#[should_panic(expected = "UndoScope mutated without register")]
#[cfg(debug_assertions)]
fn undo_scope_drop_without_register_panics() {
    let (mut r, _recv) = new_rollback();
    r.new_transaction();
    let mut scope = r.ecs.undo_scope();
    let _ = scope.entities.deref_mut().insert(());
    drop(scope);
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p rollback --test log_model`
Expected: compile FAIL — `undo_scope` not found.

- [ ] **Step 3: Implement**

Replace the `"struct Delayed Undo<T>"`, `"impl DelayedUndo<T>"`, `"DelayedUndo Deref"`, `"DelayedUndo DerefMut"` macro items:

```rust
pub struct UndoScope<'a, T> where T: /* Undo bounds */ {
    pre_hash: u32,
    touched: bool,
    registered: bool,
    value: &'a mut Undo<T>,
}

impl<T> Undo<T> where T: /* bounds */ {
    pub fn undo_scope(&mut self) -> UndoScope<'_, T> {
        UndoScope { pre_hash: unsafe { self.hash_data() }, touched: false, registered: false, value: self }
    }
}

impl<'a, T> UndoScope<'a, T> where T: /* bounds */ {
    pub fn register(mut self, f: impl FnOnce(&mut T, &::crossbeam::channel::Sender<crate::GameDataUpdate>) + 'static + Send) {
        self.registered = true;
        let mut global = self.value.global_log.lock().unwrap();
        let trans = self.value.info.current.load(::std::sync::atomic::Ordering::SeqCst);
        let wrap = self.value.wrap.expect("Undo field not wired");
        global.log.push_back(Entry { transaction: trans, undo: UndoOp::Opaque(wrap(Box::new(f))), pre_hash: self.pre_hash });
    }
}

impl<'a, T> ::std::ops::Deref for UndoScope<'a, T> where T: /* bounds */ {
    type Target = T;
    fn deref(&self) -> &T { &self.value }
}
impl<'a, T> ::std::ops::DerefMut for UndoScope<'a, T> where T: /* bounds */ {
    fn deref_mut(&mut self) -> &mut T { self.touched = true; &mut self.value }
}
impl<'a, T> Drop for UndoScope<'a, T> where T: /* bounds */ {
    fn drop(&mut self) {
        debug_assert!(!self.touched || self.registered, "UndoScope mutated without register()");
    }
}
```

Migrate `create_entity_safe` (`crates/rollback/src/rollback.rs`) — same shape, new guard:

```rust
pub fn create_entity_safe(&mut self) -> EntityKey {
    // SlotMap hashes slot versions and the free list — undo restores a
    // snapshot until the slotmapd fork exposes true inverses (Phase 2).
    let old: Ecs = (**self).clone();
    let mut scope = self.undo_scope();
    let key = scope.entities.deref_mut().insert(());
    scope.camera.insert(key, None);
    scope.isometry.insert(key, None);
    scope.rigidbody.insert(key, None);
    scope.chunk.insert(key, None);
    scope.register(move |d, s| {
        *d = old;
        s.send(GameDataUpdate::new(
            GameDataTransactionKind::Do,
            crate::GameDataUpdateKind::RemoveEntity(key),
        ))
        .unwrap();
    });
    self.send(GameDataUpdate::new(
        GameDataTransactionKind::Do,
        crate::GameDataUpdateKind::CreateEntity(key),
    ));
    key
}
```

`register` consumes the scope, ending the borrow — the trailing `self.send` stays valid. `change()` keeps existing (it's undo-before-mutate, already safe); grep for any other `delayed_undo` callers (`grep -rn delayed_undo crates/game crates/rollback crates/client crates/server`) — Task 4 already removed the `region.rs` ones; fix any stragglers the same way.

- [ ] **Step 4: Run tests + build**

Run: `cargo test -p rollback --test log_model --test rollback_restore --test hash_restore && cargo build --workspace --bins`
Expected: all PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/macros/src/lib.rs crates/rollback/src/rollback.rs crates/rollback/tests/log_model.rs
git commit -m "feat: UndoScope guard replaces delayed_undo; drop-without-register debug assert"
```

---

### Task 6: Randomized invariant test + end-to-end smoke run

**Files:**
- Create: `crates/rollback/tests/random_ops.rs`
- Test: full suite + live run

**Interfaces:**
- Consumes: everything above.
- Produces: the seeded randomized apply→rollback→hash test the spec requires; confirmation the game still runs.

- [ ] **Step 1: Write the randomized test**

```rust
//! Seeded random op sequences across transactions; rolling everything back
//! must restore the exact state hash. This is the core invariant of the
//! rollback system.
use std::hash::Hash;

use rollback::{ChunkCoords, EntityKey, Rollback};

fn state_hash(r: &Rollback) -> u32 {
    let mut hasher = crc32fast::Hasher::new();
    r.hash(&mut hasher);
    hasher.finalize()
}

#[test]
fn random_op_sequences_roll_back_to_every_boundary() {
    for seed in 0u64..8 {
        let mut rng = oorandom::Rand64::new(seed as u128);
        let (send, _recv) = crossbeam::channel::unbounded();
        let mut r = Rollback::new(Some(send));
        let mut boundaries = vec![state_hash(&r)];

        for _tx in 0..5 {
            r.new_transaction();
            for _op in 0..(1 + rng.rand_range(0..4)) {
                match rng.rand_range(0..5) {
                    0 => r.tick.update(|t| *t += 1),
                    1 => r.next_game_event_id.update(|n| *n = n.wrapping_add(3)),
                    2 => {
                        let k = rng.rand_range(0..3) as usize;
                        r.player_entites.insert(k, EntityKey::default());
                    }
                    3 => {
                        let k = rng.rand_range(0..3) as usize;
                        r.player_entites.remove(&k);
                    }
                    _ => {
                        r.create_mesh(ChunkCoords::new(0, 0, 0));
                    }
                }
            }
            boundaries.push(state_hash(&r));
        }

        for expected in boundaries.iter().rev().skip(1) {
            r.rollback();
            assert_eq!(*expected, state_hash(&r), "seed {seed}: boundary mismatch");
        }
    }
}
```

Note: `oorandom` is a workspace dep; add `oorandom = { workspace = true }` to `crates/rollback/Cargo.toml` `[dev-dependencies]` (create the section if absent).

- [ ] **Step 2: Run it**

Run: `cargo test -p rollback --test random_ops`
Expected: PASS. Any failure here is a real undo bug — debug it (the failing seed reproduces deterministically), do not weaken the test.

- [ ] **Step 3: End-to-end smoke run**

```bash
cargo build --workspace --bins
./target/debug/server > /tmp/server.log 2>&1 &
SERVER_PID=$!
./target/debug/client > /tmp/client.log 2>&1 &
CLIENT_PID=$!
sleep 15
grep -c "panicked\|Hash verification" /tmp/client.log /tmp/server.log   # expect 0 and 0
grep -c "Region recieved and loaded!" /tmp/client.log                    # expect >= 1
kill $CLIENT_PID $SERVER_PID
```

Expected: zero panics/hash failures, region loads. (Server may panic on `SendError` at shutdown when the client is killed first — known pre-existing issue at `crates/server/src/main.rs:210`, ignore panics that occur only after the `kill`.)

- [ ] **Step 4: Final full check + commit**

Run: `cargo test -p rollback --test log_model --test rollback_restore --test hash_restore --test random_ops && cargo build --workspace --bins`

```bash
git add crates/rollback/tests/random_ops.rs crates/rollback/Cargo.toml
git commit -m "test: seeded randomized rollback invariant suite; phase 1 complete"
```

---

## After this plan

Phase 2 (slotmapd fork inverse ops + `UndoSlotMap`/`UndoSparseSecondary`), Phase 3 (auto-emit `#[emit]`), Phase 4 (rapier fork), Phase 5 (remove `DerefMut`/`undo()` from the public surface) each get their own plan once this one is merged and the macro's real shape is known.
