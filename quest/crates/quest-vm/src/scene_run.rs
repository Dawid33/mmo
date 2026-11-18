//! The scene runner (spec §5.11 "Scenes", "Pacing"): a scene or a bark played by its cast, with timed lines,
//! and the ambient schedule's casting.
//!
//! Like the conversation runner (`dialogue_run`), it never touches a live VM: it reads graphs and evaluates
//! pure fns (over the cast) through a [`DialogueEnv`], and returns what happened as a [`SceneOut`] of values.
//! A [`ScenePlay`] is plain data, hashed with [`StateHash`]: carried state of its **lead** (the first cast
//! member), saved and rolled back by the caller like any region state; the [`Records`] a tick takes are the
//! lead's.
//!
//! **Timeline.** A play starts at its start tick. Each **beat** walks steps until a line is said, a
//! `pause`, a follow-up or the end: a line is said at its beat's tick and holds for its `ticks` (the
//! compiler's duration, or `(ticks=n)`), so the next beat is that many ticks later; `pause n` waits `n`
//! ticks. Lines, `=>` groups (by the scene's `@pick`), `when`, `once` and `emit` are walked as in a
//! conversation, at detour depth 1; `->` jumps to a node group (an empty one ends the scene), `return`
//! ends it. The end of its last node is its end, `Finished`, when its last line has been held. A tick past
//! several beats walks each of them in order at its own tick; its [`SceneOut`] then carries all their
//! lines as one burst, in order, without each line's own tick (a caller that ticks every tick never sees
//! one).
//!
//! **Follow-ups.** A scene's `-> other_scene` ends it `Finished` and plays `other_scene` with the cast's
//! first members (by parameter position); its first beat is walked by the next tick, at the follow-up's
//! own tick. One call ends at most one scene. A play takes at most [`SCENE_FOLLOW_UPS_MAX`] follow-ups; the
//! one past it, or one to a scene that is gone or casts more than it has, ends the play.
//!
//! **Ends.** [`ScenePlay::interrupt`] (a player talks to a cast member) ends an interruptible scene
//! `Interrupted`, and is refused by an `uninterruptible` one; [`ScenePlay::actor_lost`] (a cast member died,
//! left `HALO_BLOCKS` of the lead, or was removed) ends it `ActorLost`. A play started with fewer cast
//! members than its scene's cast ends `ActorLost` at its first beat (more are cut to the scene's cast); one
//! of no scene ends `Finished`. `SceneEnd` has no `TooDeep`: a call that walks [`BATCH_STEPS_MAX`] steps
//! without the clock moving (no line or pause that takes a tick: a line-less loop, `(ticks=0)` lines or
//! `pause 0` in a loop) ends the scene `Finished`.
//!
//! **Edits.** After the instance's graph is replaced, [`ScenePlay::rebind`] finds the scene again by its
//! qualified name and its position by its path (the node's name and member index, then each nested block
//! by its index), as a conversation does. A scene or node that is gone ends the play `Finished` (the
//! language's `SceneEnd` has no `Edited`); a path that no longer resolves restarts its node.
//!
//! **Bounded.** One call walks at most [`BATCH_STEPS_MAX`] steps; a late tick that has more to catch up on
//! goes on at the next call. One call makes at most `RUNNER_EVALS_MAX` evaluations: the one past it ends the
//! scene `Finished` when the clock has not moved in the call; after it moved, the beat being walked is put
//! back as it was when it began (its frame, the records, its lines and emits) and walked again at the next
//! call.
//!
//! **Hearing** (lines go to the clients within `HEAR_RANGE_BLOCKS` of the speaker) and **one activity per
//! NPC** (a scene is cast only from idle people) are the caller's. A bark (`bark(npc, d)`) is a play of a
//! one-actor scene.

use std::collections::{BTreeMap, BTreeSet};
use std::hash::Hasher;

use state_hash::StateHash;

use crate::consts::{BATCH_STEPS_MAX, SCENE_FOLLOW_UPS_MAX};
use crate::dialogue::{DialogueKind, StepDef};
use crate::dialogue_eval::{
    Advance, Common, Eval, Frame, Rebound, advance, find, group, rebind_frame, spent, with_def,
};
use crate::dialogue_run::{Attachment, DialogueEnv, Emitted, InstanceId, Records, SaidLine};
use crate::pure::PureFault;
use crate::wire::WireValue;

/// Why a scene ended (the host's `SceneEnd`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum SceneReason {
    Finished,
    Interrupted,
    ActorLost,
}

/// A scene's end: the host raises `SceneEnded { scene, reason }` from it to `instance`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SceneEnd {
    pub instance: InstanceId,
    /// The scene's index in its graph, and its qualified name (empty when the index names nothing).
    pub scene: u16,
    pub name: String,
    pub reason: SceneReason,
}

/// What one call produced, in order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SceneOut {
    pub lines: Vec<SaidLine>,
    pub emits: Vec<Emitted>,
    pub ended: Option<SceneEnd>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    /// The scene's first beat opens it (its entry node picked).
    Opening,
    Playing,
    Ended,
}

/// A scene being played by its cast.
#[derive(Clone, Debug, PartialEq)]
pub struct ScenePlay {
    instance: InstanceId,
    scene: u16,
    name: String,
    cast: Vec<WireValue>,
    interruptible: bool,
    /// The node being walked (`None` while opening and once ended).
    frame: Option<Frame>,
    state: State,
    /// The tick of the next beat.
    next_at: u64,
    follow_ups: u32,
    /// Fault sites reported: (instance, fn), or (instance, `EMIT_SITE | event`) for an emit.
    reported: BTreeSet<(InstanceId, u32)>,
}

/// What one beat decided.
enum Beat {
    /// The next beat is at this tick.
    Wait(u64),
    /// The call's step budget ran out mid-beat: go on at the next call.
    Yield,
    End(SceneReason),
    FollowUp(u16),
}

impl ScenePlay {
    /// Instance `i` plays its scene `scene` (an index into its graph's dialogues) with `cast` (by
    /// parameter position, the lead first), from tick `now` (its first beat).
    pub fn start(
        env: &mut dyn DialogueEnv,
        i: InstanceId,
        scene: u16,
        cast: Vec<WireValue>,
        now: u64,
    ) -> ScenePlay {
        let mut p = ScenePlay {
            instance: i,
            scene,
            name: String::new(),
            cast,
            interruptible: true,
            frame: None,
            state: State::Opening,
            next_at: now,
            follow_ups: 0,
            reported: BTreeSet::new(),
        };
        p.describe(&*env);
        p
    }

    /// The clock reached `now`: every beat due is walked (module docs).
    pub fn tick(&mut self, env: &mut dyn DialogueEnv, rec: &mut Records, now: u64) -> SceneOut {
        let mut out = SceneOut::default();
        let mut budget = 0u32;
        let mut evals = 0u32;
        let mut fresh = true;
        while self.state != State::Ended && now >= self.next_at {
            let base = self.next_at;
            // A beat cut short by the evaluation cap is walked again from here at the next call.
            let begun = (!fresh).then(|| {
                (
                    self.frame.clone(),
                    rec.clone(),
                    out.lines.len(),
                    out.emits.len(),
                )
            });
            match self.beat(env, rec, base, &mut out, &mut budget, &mut evals, fresh) {
                Beat::Wait(at) => {
                    self.next_at = at;
                    // Only a wait that moves the clock: a zero-tick loop must not pass for progress.
                    if at > base {
                        fresh = false;
                    }
                }
                Beat::Yield => {
                    if spent(evals)
                        && let Some((frame, records, lines, emits)) = begun
                    {
                        // What the beat evaluated since the cap is not to be trusted.
                        self.frame = frame;
                        *rec = records;
                        out.lines.truncate(lines);
                        out.emits.truncate(emits);
                    }
                    break;
                }
                Beat::End(reason) => self.end(reason, &mut out),
                Beat::FollowUp(target) => {
                    self.end(SceneReason::Finished, &mut out);
                    self.follow(&*env, target, base);
                    // One end per call: the follow-up's first beat is the next call's.
                    break;
                }
            }
        }
        out
    }

    /// After an edit (the env's graphs replaced): the scene and its position found again (module docs).
    /// `Some` with the end when the scene or its node is gone; `None` when the play goes on (or had ended).
    pub fn rebind(&mut self, env: &mut dyn DialogueEnv) -> Option<SceneOut> {
        if self.state == State::Ended {
            return None;
        }
        let found = env
            .graph(self.instance)
            .and_then(|g| find(g, &self.name))
            .map(|(k, _)| k);
        let kept = match (found, self.frame.as_mut()) {
            (None, _) => false,
            (Some(_), Some(f)) => !matches!(rebind_frame(&*env, f), Rebound::Gone),
            (Some(_), None) => true,
        };
        if !kept {
            let mut out = SceneOut::default();
            self.end(SceneReason::Finished, &mut out);
            return Some(out);
        }
        if let Some(k) = found {
            self.scene = k;
        }
        self.describe(&*env);
        None
    }

    /// A player talks to a cast member: an interruptible scene ends `Interrupted` (nothing more once it
    /// has ended); `None` when it is `uninterruptible` (the cast is busy).
    pub fn interrupt(&mut self) -> Option<SceneOut> {
        if !self.interruptible {
            return None;
        }
        let mut out = SceneOut::default();
        self.end(SceneReason::Interrupted, &mut out);
        Some(out)
    }

    /// A cast member died, left range or was removed: the end (`ActorLost`); nothing once it has ended.
    pub fn actor_lost(&mut self) -> SceneOut {
        let mut out = SceneOut::default();
        self.end(SceneReason::ActorLost, &mut out);
        out
    }

    /// Whether a talk to a cast member ends the scene (else the cast is busy).
    pub fn interruptible(&self) -> bool {
        self.interruptible
    }

    pub fn ended(&self) -> bool {
        self.state == State::Ended
    }

    pub fn instance(&self) -> InstanceId {
        self.instance
    }

    /// The scene playing now (after a follow-up, the follow-up).
    pub fn scene(&self) -> u16 {
        self.scene
    }

    /// The cast, the lead first.
    pub fn cast(&self) -> &[WireValue] {
        &self.cast
    }

    /// The scene's name and interruptibility, from its graph.
    fn describe(&mut self, env: &dyn DialogueEnv) {
        let d = env
            .graph(self.instance)
            .and_then(|g| g.dialogues.get(usize::from(self.scene)));
        self.name = d.map(|d| d.name.clone()).unwrap_or_default();
        self.interruptible = d.is_none_or(|d| d.interruptible);
    }

    fn end(&mut self, reason: SceneReason, out: &mut SceneOut) {
        if self.state == State::Ended {
            return;
        }
        self.state = State::Ended;
        self.frame = None;
        out.ended = Some(SceneEnd {
            instance: self.instance,
            scene: self.scene,
            name: self.name.clone(),
            reason,
        });
    }

    /// After the current scene ended at its `-> target` (at tick `at`): `target` plays with the cast.
    fn follow(&mut self, env: &dyn DialogueEnv, target: u16, at: u64) {
        if self.follow_ups >= SCENE_FOLLOW_UPS_MAX {
            return;
        }
        let fits = env
            .graph(self.instance)
            .and_then(|g| g.dialogues.get(usize::from(target)))
            .is_some_and(|d| {
                d.kind == DialogueKind::Scene && usize::from(d.cast) <= self.cast.len()
            });
        if !fits {
            return;
        }
        self.scene = target;
        self.describe(env);
        self.follow_ups += 1;
        self.state = State::Opening;
        self.next_at = at;
    }

    /// Opens the scene: its cast checked against the scene's, its entry node picked.
    fn open(
        &mut self,
        env: &mut dyn DialogueEnv,
        rec: &mut Records,
        at: u64,
        evals: &mut u32,
    ) -> Result<(), SceneReason> {
        let want = env
            .graph(self.instance)
            .and_then(|g| g.dialogues.get(usize::from(self.scene)))
            .filter(|d| d.kind == DialogueKind::Scene && d.name == self.name)
            .map(|d| usize::from(d.cast))
            .ok_or(SceneReason::Finished)?;
        if self.cast.len() < want {
            return Err(SceneReason::ActorLost);
        }
        self.cast.truncate(want);
        let (i, name) = (self.instance, self.name.clone());
        let node = Eval {
            env: &mut *env,
            rec,
            cast: &self.cast,
            reported: &mut self.reported,
            now: at,
            evals: &mut *evals,
        }
        .entry(i, &name, 1)
        .filter(|_| !spent(*evals))
        .ok_or(SceneReason::Finished)?;
        self.frame = with_def(&*env, i, &name, |di, d| Frame::at(i, di, d, node)).flatten();
        if self.frame.is_none() {
            return Err(SceneReason::Finished);
        }
        self.state = State::Playing;
        Ok(())
    }

    /// One beat at tick `at` (module docs). `budget` counts the call's steps, `evals` its evaluations
    /// (past `RUNNER_EVALS_MAX` the scene ends `Finished`, or yields once the clock moved); `fresh`: the
    /// clock has not moved in this call yet.
    #[allow(clippy::too_many_arguments)]
    fn beat(
        &mut self,
        env: &mut dyn DialogueEnv,
        rec: &mut Records,
        at: u64,
        out: &mut SceneOut,
        budget: &mut u32,
        evals: &mut u32,
        fresh: bool,
    ) -> Beat {
        if self.state == State::Opening
            && let Err(reason) = self.open(env, rec, at, evals)
        {
            return Beat::End(reason);
        }
        loop {
            if spent(*evals) {
                // A late catch-up goes on at the next call (`tick` puts the beat back as it began).
                return if fresh {
                    Beat::End(SceneReason::Finished)
                } else {
                    Beat::Yield
                };
            }
            if *budget >= BATCH_STEPS_MAX {
                // A whole call's steps without the clock moving: a loop that never takes a tick.
                return if fresh {
                    Beat::End(SceneReason::Finished)
                } else {
                    Beat::Yield
                };
            }
            *budget += 1;
            let Some(f) = self.frame.as_mut() else {
                return Beat::End(SceneReason::Finished);
            };
            let step = match advance(&*env, f) {
                Advance::Step(s) => s,
                Advance::Popped => continue,
                // The end of its node; or its steps are gone.
                Advance::Empty | Advance::Missing => return Beat::End(SceneReason::Finished),
            };
            let (i, name) = (f.instance, f.name.clone());
            let mut ev = Eval {
                env: &mut *env,
                rec: &mut *rec,
                cast: &self.cast,
                reported: &mut self.reported,
                now: at,
                evals: &mut *evals,
            };
            let step = match ev.common_step(f, step, 1, &mut out.lines, &mut out.emits) {
                Common::Said(ticks) => return Beat::Wait(at.saturating_add(u64::from(ticks))),
                Common::Next => continue,
                Common::Other(step) => step,
            };
            match step {
                StepDef::Pause { ticks } => return Beat::Wait(at.saturating_add(u64::from(ticks))),
                StepDef::Return { .. } => return Beat::End(SceneReason::Finished),
                StepDef::FollowUp { scene } => return Beat::FollowUp(scene),
                StepDef::Jump { group: g } => {
                    let Some(g) = group(&*ev.env, i, &name, usize::from(g)) else {
                        return Beat::End(SceneReason::Finished);
                    };
                    let Some(node) = ev.pick_node(i, &g, 1) else {
                        return Beat::End(SceneReason::Finished);
                    };
                    self.frame =
                        with_def(&*env, i, &name, |di, d| Frame::at(i, di, d, node)).flatten();
                }
                // Not in a scene (the graph refuses them): walked past.
                StepDef::Choice { .. } | StepDef::Detour { .. } => {}
                // Walked by `common_step` above; never here.
                StepDef::Line { .. }
                | StepDef::Group { .. }
                | StepDef::When { .. }
                | StepDef::Once { .. }
                | StepDef::Emit { .. } => {}
            }
        }
    }
}

/// A scene candidate's rank, the smallest first: (score negated, the tick it was last played (never
/// first), qualified name, program key, instance, scene index).
type AmbientRank = (i32, Option<u64>, String, Option<u64>, InstanceId, u16);

/// Ambient casting: the best candidate by score → least recently played → stable key, its cast bound from
/// `idle` in order, where the scene's `where` holds. Deterministic given its inputs.
///
/// `candidates` are attachments of scenes (anything else is ignored), in any order; `idle` the idle people
/// near enough, in the caller's (deterministic) order (a repeated person counts once, at its first place);
/// `played` the tick each scene was last played, keyed by (its program's key, `DialogueEnv::program_key`,
/// and its qualified name): two programs' scenes of one name do not share recency. A candidate's cast is
/// the first tuple of distinct idle people, in the order of their positions in `idle`, for which its
/// `where` holds (no `where`: the first). At most
/// [`BATCH_STEPS_MAX`] `where` evaluations are made in all; a fault counts as false and is reported once per
/// fn. `_now` is reserved for a random tie-break (none today).
pub fn cast_ambient(
    env: &mut dyn DialogueEnv,
    candidates: &[Attachment],
    idle: &[WireValue],
    played: &BTreeMap<(Option<u64>, String), u64>,
    _now: u64,
) -> Option<(Attachment, Vec<WireValue>)> {
    let mut ranked: Vec<(AmbientRank, usize, Option<u16>)> = candidates
        .iter()
        .filter_map(|a| {
            let d = env
                .graph(a.instance)?
                .dialogues
                .get(usize::from(a.dialogue))?;
            (d.kind == DialogueKind::Scene).then(|| {
                let key = env.program_key(a.instance);
                let rank = (
                    -i32::from(d.score),
                    played.get(&(key, d.name.clone())).copied(),
                    d.name.clone(),
                    key,
                    a.instance,
                    a.dialogue,
                );
                (rank, usize::from(d.cast), d.cast_cond)
            })
        })
        .collect();
    ranked.sort();
    ranked.dedup();
    let mut people: Vec<WireValue> = Vec::with_capacity(idle.len());
    for p in idle {
        if !people.contains(p) {
            people.push(p.clone());
        }
    }
    let mut b = Binder {
        env,
        idle: &people,
        evals: 0,
        reported: BTreeSet::new(),
    };
    for ((_, _, _, _, instance, dialogue), n, cond) in ranked {
        let mut cast = Vec::with_capacity(n);
        if n <= b.idle.len() && b.bind(instance, cond, n, &mut cast) {
            return Some((Attachment { instance, dialogue }, cast));
        }
        if b.evals >= BATCH_STEPS_MAX {
            return None;
        }
    }
    None
}

/// The search for a candidate's cast.
struct Binder<'a> {
    env: &'a mut dyn DialogueEnv,
    idle: &'a [WireValue],
    evals: u32,
    reported: BTreeSet<(InstanceId, u16)>,
}

impl Binder<'_> {
    /// Extends `cast` to `n` distinct idle people for whom `cond` holds, the first in idle order; `false`
    /// when there are none (or the evaluations ran out).
    fn bind(
        &mut self,
        i: InstanceId,
        cond: Option<u16>,
        n: usize,
        cast: &mut Vec<WireValue>,
    ) -> bool {
        if cast.len() == n {
            return self.holds(i, cond, cast);
        }
        for p in self.idle {
            if self.evals >= BATCH_STEPS_MAX {
                return false;
            }
            if cast.contains(p) {
                continue;
            }
            cast.push(p.clone());
            if self.bind(i, cond, n, cast) {
                return true;
            }
            cast.pop();
        }
        false
    }

    fn holds(&mut self, i: InstanceId, cond: Option<u16>, cast: &[WireValue]) -> bool {
        let Some(f) = cond else {
            return true;
        };
        self.evals += 1;
        let fault = match self.env.eval(i, f, cast, 1) {
            Ok(WireValue::Bool(b)) => return b,
            Ok(_) => PureFault::BadCall,
            Err(e) => e,
        };
        if self.reported.insert((i, f)) {
            self.env.report(i, fault);
        }
        false
    }
}

// ---- StateHash -----------------------------------------------------------------------------------------

impl StateHash for SceneReason {
    fn state_hash<H: Hasher>(&self, h: &mut H) {
        h.write_u8(match self {
            SceneReason::Finished => 0,
            SceneReason::Interrupted => 1,
            SceneReason::ActorLost => 2,
        });
    }
}

impl StateHash for SceneEnd {
    fn state_hash<H: Hasher>(&self, h: &mut H) {
        self.instance.state_hash(h);
        self.scene.state_hash(h);
        self.name.state_hash(h);
        self.reason.state_hash(h);
    }
}

impl StateHash for SceneOut {
    fn state_hash<H: Hasher>(&self, h: &mut H) {
        self.lines.state_hash(h);
        self.emits.state_hash(h);
        self.ended.state_hash(h);
    }
}

impl StateHash for State {
    fn state_hash<H: Hasher>(&self, h: &mut H) {
        h.write_u8(match self {
            State::Opening => 0,
            State::Playing => 1,
            State::Ended => 2,
        });
    }
}

impl StateHash for ScenePlay {
    fn state_hash<H: Hasher>(&self, h: &mut H) {
        self.instance.state_hash(h);
        self.scene.state_hash(h);
        self.name.state_hash(h);
        self.cast.state_hash(h);
        self.interruptible.state_hash(h);
        self.frame.state_hash(h);
        self.state.state_hash(h);
        self.next_at.state_hash(h);
        self.follow_ups.state_hash(h);
        self.reported.state_hash(h);
    }
}
