# Handoff — quest construction: syntax and semantics research

> **Done 2026-11-16.** Findings and proposals: `docs/superpowers/specs/2026-11-16-quest-construction-design.md`,
> accepted by Dawid the same day. Worked examples: `docs/superpowers/specs/2026-11-16-quest-examples.md`.
> Still to do: fold the accepted constructs into the language spec §5.11–5.13 and the MVP plan's Phase 10,
> timed with the session implementing them.

Date: 2026-11-16. For the next context, which researches how quests (and the dialogue they use) are built in
quest script: syntax and semantics. Nothing here is built; quest-script implementation (the standalone MVP) is
under way in another session.

## Rule for this work

**Do not edit the quest-script language spec or its MVP plan** — another session is implementing from them
(Dawid, 2026-11-16):

- `docs/superpowers/specs/2026-11-16-quest-script-language-design.md`
- `docs/superpowers/plans/2026-11-16-quest-script-language-mvp.md`

Write findings and proposals in a new doc (e.g. `docs/superpowers/specs/2026-11-1x-quest-construction-design.md`)
and ask Dawid before anything is folded into the spec.

## Decided by Dawid (2026-11-16)

- **Direction** (`docs/game-design.md`, revision 2026-11-16):
  - one first-person character;
  - quests are authored in **packages**, never made in live play;
  - three quest types — repeatable, one-off and world-line;
  - acts are quest-gated in single player and time-gated (not time-started) in multiplayer;
  - editor mode is admin-only;
  - one quest namespace per quest instance, owned by the system.
- **Recursion:** only a script function calling itself (directly or through others) is forbidden. Recursive
  data and recursive dialogue are fine, run as loops. The Rust implementation has no such rule.
- **Dialogue is declarative.** Quest script describes dialogue and attaches it to NPCs. The VM does not run it;
  the **region's Rust code runs the conversation**. Conditions are **pure VM functions** the runner calls.
  Effects are **events** (`emit`) handled by ordinary `@on` handlers.
- **Attached dialogues combine commutatively:** the result depends only on the set attached to an NPC.
- **Conversations must work across region boundaries.**
- **Scripts have access to region data**, abstracted so that undo and rollback are transparent to them.

## Proposed, not approved (judge, keep or replace)

- **Quest syntax** (spec §5.12):
  - `quest Name:`, with roles (`role farmer: Person where <requirement>`);
  - stages (a persistent enum migrated by name) with stage-scoped handlers and dialogues, and `-> Stage`;
  - `objective "text" @id at <target>`;
  - the builtins `complete`, `fail` and `decline`.
- **Dialogue syntax and semantics** (spec §5.11, §5.13):
  - steps, not statements;
  - nodes and node groups with Yarn-style saliency;
  - `choice:` with timeouts, `default` options and a hidden `default:` fallback;
  - line groups `=>`, `when` / `once [per p]`, `detour`, `emit`;
  - line ids `@path` and a string table per locale;
  - `for <npc>`, plus `topic` / `priority` for the shared menu;
  - barks `(npc)`.
- **Across seams** (spec §5.11 "Across region boundaries", §9.1):
  - a conversation is carried state of the NPC;
  - picks and `emit`s travel as stamped messages;
  - attaching script instances mirror their conditions' read set to the NPC's region, like the halo;
  - script instances are anchored to a region or an entity.
- **Quest lifecycle** (`docs/superpowers/specs/2026-11-16-quests-in-the-world-design.md`):
  - definitions, then spawn jobs, then placement with an idempotent failure path;
  - completion, then reaping;
  - acts and story state;
  - requirements as declarative queries, partly over region snapshots.
- **Packages** (`docs/superpowers/specs/2026-11-16-content-packages-design.md`).

## Read first

1. `docs/game-design.md` — the direction.
2. The language spec §5.11–5.13 and §9.1, as context only (do not edit).
3. `docs/superpowers/specs/2026-11-16-dialogue-and-quest-structure-research.md` — the survey: ink, Yarn Spinner
   3, Bethesda's quest stages/aliases/Story Manager, Witcher 3 quest graphs, storylets, statecharts. Its
   execution model is superseded (the note at its §5).
4. `docs/superpowers/specs/2026-11-16-dialogue-examples.md` — Yarn samples converted, plus Errantry examples.
5. `docs/superpowers/specs/2026-11-16-quests-in-the-world-design.md`.
6. Engine facts for seams: `crates/game/src/world_view.rs` (own region plus 8-block halo, one tick old,
   rollback state), `intents.rs` (applied T+1), `migrate.rs` (what crosses a seam), `history/records.rs:48`
   (`Person`: race, sex, birth, family, citizenship, location — no occupation or tags).

## Open questions worth researching

1. **Quest structure beyond a flat stage enum:** parallel objectives, nested stages, optional branches,
   failure and retry, multi-region quests (one instance per region coordinating via the namespace?).
2. **Person selection:** roles bound at spawn; a proposed `for npc where <predicate>` filter for generic
   dialogue; what person data predicates need (occupation first, then tags, faction, relationships with
   players).
3. **Ambient NPC-to-NPC scenes:** a multi-speaker dialogue form with no choices; who starts it; interruption.
4. **Line pacing:** whether the runner waits for "continue" or uses durations, and how line timing is
   derived deterministically.
5. **Requirements:** which expressions a role's `where` may use (a query subset the coordinator can index);
   whether the requirement syntax and dialogue `when` should share one scoring rule.
6. **`world.region_query`** (spec §9.1) collides with the namespace's `world.*`: rename it (`regions.query`)
   or drop it.
7. **Repeatable quests:** what a reset clears, whether completions are recorded in history.
8. **Return values from dialogue,** and whether quest progress ever needs a conversation's outcome other than
   through events.
