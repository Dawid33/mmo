//! Who may change what (spec §5.11–5.12), over the typed call graph (`callgraph::facts`):
//!
//! - **Purity** (`impure-condition`): every synthesised pure fn (`hir::Fn::pure`: a condition, a
//!   substitution, an `emit` argument, a target, a speaker, a `once per` key, a filter, a scene's or a role's
//!   `where`, a `requires`, a transition), and every `fn` it calls, transitively, assigns nothing but its
//!   own locals, calls no host command or request, no compiler builtin, no objective command, and does not
//!   `await`; and no error escapes the synthesised fn itself (a fallible call handled inside is fine). The
//!   message names the call chain to the offence.
//! - **Package-level content** (`stateless-dialogue`): a dialogue with `for npc where` or a scene, declared
//!   outside any quest, reads no script global (through what it calls too) and does not `emit`.
//! - **Generated quest state is read-only** (`role-assign`): a role, `stage`, `participants` or an objective
//!   is never assigned, changed in place or referred to by `&` in any code; only the builtins and an
//!   objective's commands change them.
//! - **A part never writes quest state** (`part-write`): a part's handlers, and what they call, assign no
//!   quest global and no top-level global (home state, as quest `let`s; its own `let`s are its own), call no objective command, no `join`, `leave`, `complete` or
//!   `fail`, and no `-> Stage`. Reported at the handler's site, naming the chain.

use crate::ast::Span;
use crate::callgraph::{self, Effect, Facts, Hit};
use crate::diag::Diagnostic;
use crate::hir::{Crate, DefId, DefKind, PureSite, QuestFn, StepKind};
use crate::infer::Typed;
use quest_vm::host::{HostApi, HostFnKind};
use std::collections::{BTreeMap, BTreeSet};

/// The Phase 10 write checks: `impure-condition`, `stateless-dialogue`, `role-assign`, `part-write`.
pub fn purity(krate: &Crate, typed: &Typed, host: &HostApi) -> Vec<Diagnostic> {
    purity_with(krate, typed, host, &callgraph::facts(krate, typed))
}

/// [`purity`] over the typed call graph `facts` (`callgraph::facts`), computed once by `infer`.
pub(crate) fn purity_with(
    krate: &Crate,
    typed: &Typed,
    host: &HostApi,
    facts: &BTreeMap<DefId, Facts>,
) -> Vec<Diagnostic> {
    let mut c = Checker {
        krate,
        host,
        facts,
        generated: generated(krate),
        diags: Vec::new(),
    };
    for (i, def) in krate.defs.iter().enumerate() {
        let d = DefId(u32::try_from(i).unwrap_or(u32::MAX));
        match &def.kind {
            DefKind::Fn(f) => {
                if let Some(site) = f.pure {
                    c.pure_fn(d, site, typed);
                }
                c.generated_writes(d);
            }
            DefKind::Dialogue(dl) if dl.stateless => c.stateless(d, dl),
            DefKind::Quest(q) => c.parts(q),
            _ => {}
        }
    }
    c.diags
}

struct Checker<'a> {
    krate: &'a Crate,
    host: &'a HostApi,
    facts: &'a BTreeMap<DefId, Facts>,
    /// The globals only the builtins and objective commands change ([`generated`]).
    generated: BTreeSet<DefId>,
    diags: Vec<Diagnostic>,
}

/// The globals only the builtins and objective commands change: every quest's roles, `stage`,
/// `participants` and objectives.
fn generated(krate: &Crate) -> BTreeSet<DefId> {
    let mut out = BTreeSet::new();
    for def in &krate.defs {
        if let DefKind::Quest(q) = &def.kind {
            out.extend(q.roles.iter().map(|r| r.global));
            out.extend([q.generated.stage, q.generated.participants]);
            out.extend(
                q.stages
                    .iter()
                    .flat_map(|s| &s.objectives)
                    .map(|o| o.global),
            );
        }
    }
    out
}

/// What a synthesised fn computes, for messages.
fn site_noun(site: PureSite) -> &'static str {
    match site {
        PureSite::Cond
        | PureSite::Filter
        | PureSite::CastCond
        | PureSite::Where(_)
        | PureSite::Requires
        | PureSite::Transition(_) => "a condition",
        PureSite::Subst => "a line's substitution",
        PureSite::EmitArg => "an `emit` argument",
        PureSite::Target => "a target",
        PureSite::Speaker => "a line's speaker",
        PureSite::Per => "a `once per` key",
    }
}

impl Checker<'_> {
    fn file_of(&self, d: DefId) -> &str {
        self.krate
            .def(d)
            .and_then(|def| self.krate.module(def.module))
            .map_or("", |m| m.file.as_str())
    }

    fn error(&mut self, code: &'static str, message: String, of: DefId, at: Span) {
        let file = self.file_of(of).to_string();
        self.diags.push(Diagnostic::error(
            code, message, &file, at.line, at.col, at.len,
        ));
    }

    /// `, through `a → b`,` for a hit reached through calls; empty for the start's own.
    fn through<T>(&self, hit: &Hit<T>) -> String {
        if hit.chain.is_empty() {
            return String::new();
        }
        let names: Vec<String> = hit
            .chain
            .iter()
            .map(|d| self.krate.qualified_name(*d))
            .collect();
        format!(", through `{}`,", names.join(" → "))
    }

    fn host_name(&self, f: u32) -> String {
        self.host.fns.get(f as usize).map_or(String::new(), |hf| {
            match hf.namespace.as_ref().or(hf.receiver.as_ref()) {
                Some(owner) => format!("{owner}.{}", hf.name),
                None => hf.name.clone(),
            }
        })
    }

    /// What an effect does, when it is not pure: `None` for a host query.
    fn impure(&self, e: Effect) -> Option<String> {
        Some(match e {
            Effect::Write(g) => format!("assigns `{}`", self.krate.qualified_name(g)),
            Effect::Host(f) => {
                let kind = self.host.fns.get(f as usize).map(|hf| hf.kind);
                let name = self.host_name(f);
                match kind {
                    Some(HostFnKind::Command) if name == "emit" => "emits an event".to_string(),
                    Some(HostFnKind::Command) => format!("calls the host command `{name}`"),
                    Some(HostFnKind::Promise) => format!("calls the host request `{name}`"),
                    Some(HostFnKind::Query) | None => return None,
                }
            }
            Effect::Quest(f) => format!("calls `{}`", f.name()),
            Effect::Objective(op) => format!("changes an objective (`{}()`)", op.name()),
            Effect::Await => "awaits".to_string(),
            Effect::Stage => "changes the stage".to_string(),
        })
    }

    /// `impure-condition` for the synthesised fn `d`.
    fn pure_fn(&mut self, d: DefId, site: PureSite, typed: &Typed) {
        let noun = site_noun(site);
        let hit = callgraph::search(self.facts, d, |_, f| {
            f.effects
                .iter()
                .find_map(|(e, at)| self.impure(*e).map(|what| (what, *at)))
        });
        if let Some(hit) = hit {
            let through = self.through(&hit);
            self.error(
                "impure-condition",
                format!(
                    "{noun} must be pure, but this{through} {}: a dialogue changes nothing itself (emit an event, and let a handler do it)",
                    hit.what
                ),
                d,
                hit.at,
            );
        }
        if let Some(sig) = typed.sigs.get(&d)
            && !sig.raises.is_empty()
        {
            let at = self.krate.def(d).map_or(Span::default(), |def| def.span);
            self.error(
                "impure-condition",
                format!(
                    "{noun} must be pure, but an error can escape it ({}): handle it here (`… else false`)",
                    sig.raises.join(" | ")
                ),
                d,
                at,
            );
        }
    }

    /// `stateless-dialogue` for the package-level dialogue or scene `d`.
    fn stateless(&mut self, d: DefId, dl: &crate::hir::Dialogue) {
        let name = self.krate.qualified_name(d);
        for f in dl.pure_fns() {
            let hit = callgraph::search(self.facts, f, |_, facts| facts.reads.first().copied());
            if let Some(hit) = hit {
                let through = self.through(&hit);
                let global = self.krate.qualified_name(hit.what);
                self.error(
                    "stateless-dialogue",
                    format!(
                        "`{name}` is package-level content (declared outside any quest): it reads no script global, but this{through} reads `{global}`"
                    ),
                    f,
                    hit.at,
                );
            }
        }
        for step in dl.steps() {
            if let StepKind::Emit(_, _) = step.kind {
                self.error(
                    "stateless-dialogue",
                    format!(
                        "`{name}` is package-level content (declared outside any quest): no script instance receives an `emit`"
                    ),
                    d,
                    step.span,
                );
            }
        }
    }

    /// `role-assign` for every write of a generated quest global in the body of `d`.
    fn generated_writes(&mut self, d: DefId) {
        let Some(f) = self.facts.get(&d) else { return };
        let writes: Vec<(DefId, Span)> = f
            .effects
            .iter()
            .filter_map(|(e, at)| match e {
                Effect::Write(g) if self.generated.contains(g) => Some((*g, *at)),
                _ => None,
            })
            .collect();
        for (g, at) in writes {
            let name = self.krate.def(g).map_or("", |def| def.name.as_str());
            let message = format!(
                "`{name}` is read-only: roles never re-bind, and the stage, the participants and the objectives change only through `-> Stage`, the builtins (`join`, `leave`, …) and an objective's commands"
            );
            self.error("role-assign", message, d, at);
        }
    }

    /// `part-write` for every part's handlers.
    fn parts(&mut self, q: &crate::hir::Quest) {
        let part_globals: BTreeSet<DefId> =
            q.parts.iter().flat_map(|p| &p.globals).copied().collect();
        // The home's state a part may not write: the quest's `let`s and the program's top-level `let`s (both
        // live in the home instance); the generated ones are `role-assign` everywhere.
        let quest_globals: BTreeSet<DefId> = self
            .krate
            .defs
            .iter()
            .enumerate()
            .filter(|(_, def)| matches!(def.kind, DefKind::Global(_)))
            .map(|(i, _)| DefId(u32::try_from(i).unwrap_or(u32::MAX)))
            .filter(|g| !part_globals.contains(g) && !self.generated.contains(g))
            .collect();
        for p in &q.parts {
            for h in &p.handlers {
                let hits = self.part_hits(*h, &quest_globals);
                for hit in hits {
                    let through = self.through(&hit);
                    self.error(
                        "part-write",
                        format!(
                            "part `{}` never writes quest state, but this{through} {}: `emit` an event to the quest, and let its handler do it",
                            p.name, hit.what
                        ),
                        *h,
                        hit.at,
                    );
                }
            }
        }
    }

    /// Every quest-state write a part's handler `h` makes: one per site of its own body (an effect, or a
    /// call that leads to one).
    fn part_hits(&self, h: DefId, quest_globals: &BTreeSet<DefId>) -> Vec<Hit<String>> {
        let writes = |e: Effect| -> Option<String> {
            match e {
                Effect::Write(g) if quest_globals.contains(&g) => {
                    Some(format!("assigns `{}`", self.krate.qualified_name(g)))
                }
                Effect::Objective(op) => Some(format!("changes an objective (`{}()`)", op.name())),
                Effect::Quest(
                    f @ (QuestFn::Join | QuestFn::Leave | QuestFn::Complete | QuestFn::Fail),
                ) => Some(format!("calls `{}`", f.name())),
                Effect::Stage => Some("changes the stage".to_string()),
                _ => None,
            }
        };
        let Some(own) = self.facts.get(&h) else {
            return Vec::new();
        };
        let mut out: Vec<Hit<String>> = own
            .effects
            .iter()
            .filter_map(|(e, at)| {
                writes(*e).map(|what| Hit {
                    what,
                    chain: Vec::new(),
                    at: *at,
                })
            })
            .collect();
        let mut seen = BTreeSet::new();
        for (callee, at) in &own.calls {
            if !seen.insert(*callee) {
                continue;
            }
            let hit = callgraph::search(self.facts, *callee, |_, f| {
                f.effects
                    .iter()
                    .find_map(|(e, at)| writes(*e).map(|w| (w, *at)))
            });
            if let Some(mut hit) = hit {
                hit.chain.insert(0, *callee);
                hit.at = *at;
                out.push(hit);
            }
        }
        out.sort_by_key(|h| (h.at.line, h.at.col));
        out
    }
}
