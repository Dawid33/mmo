//! Read sets (spec §5.11 "Across region boundaries", §5.12 parts): the globals a mirror must carry.
//!
//! - The **dialogue read set**: every global read by a dialogue's or scene's synthesised fns (conditions,
//!   substitutions, `emit` arguments, targets, filters, speakers, `once per` keys), and by everything they
//!   call, transitively. Only these are mirrored to an NPC's region. A stage's transitions are not: the home
//!   runs them on its own state (ruling T44-1).
//! - Each **part's read set**: the home's globals (the quest's and the program's top-level `let`s, not the
//!   parts' own) its handlers read, and what they call reads.
//!
//! `mirror-size` (a warning) when a read set's static size exceeds `MIRROR_WORDS_WARN` words: each global
//! counts its slot and its value's words; a value of unbounded size (a `str`, a `list`, a `dict`, a type
//! that contains itself) counts `MIRROR_WORDS_WARN` on its own, so reading one warns. Reported at the largest
//! global of the set. The generated `participants` list is not counted (a deviation from spec §5.11, ruled
//! at Task 49): it is bounded by the party, and nearly every part reads it.

use crate::callgraph::{self, Facts};
use crate::diag::{Diagnostic, Severity};
use crate::hir::{Crate, DefId, DefKind};
use crate::infer::Typed;
use crate::ty::{Ty, TyId};
use quest_vm::consts::MIRROR_WORDS_WARN;
use std::collections::{BTreeMap, BTreeSet};

/// The globals the mirrors carry.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReadSets {
    /// Read by the dialogues' and scenes' synthesised fns (and what they call).
    pub dialogue: BTreeSet<DefId>,
    /// Each part's (by its index in `Quest::parts`): the home's globals (quest and top-level `let`s) its
    /// handlers read.
    pub parts: Vec<BTreeSet<DefId>>,
}

/// The read sets, and a `mirror-size` warning for each one that is too big, over the typed call graph
/// `facts` (`callgraph::facts`), computed once by `infer`, which keeps them in `Typed::read_sets` (and
/// reports the warnings once).
pub(crate) fn read_sets_with(
    krate: &Crate,
    typed: &Typed,
    facts: &BTreeMap<DefId, Facts>,
) -> (ReadSets, Vec<Diagnostic>) {
    let mut starts: Vec<DefId> = Vec::new();
    let mut parts: Vec<(String, Vec<DefId>)> = Vec::new();
    let mut part_globals: BTreeSet<DefId> = BTreeSet::new();
    // Not counted by `mirror-size` (module docs).
    let mut uncounted: BTreeSet<DefId> = BTreeSet::new();
    for def in &krate.defs {
        match &def.kind {
            DefKind::Dialogue(dl) => starts.extend(dl.pure_fns()),
            DefKind::Quest(q) => {
                uncounted.insert(q.generated.participants);
                for p in &q.parts {
                    parts.push((p.name.clone(), p.handlers.clone()));
                    part_globals.extend(p.globals.iter().copied());
                }
            }
            _ => {}
        }
    }
    let reads = |starts: &[DefId]| -> BTreeSet<DefId> {
        callgraph::reach(facts, starts)
            .iter()
            .filter_map(|d| facts.get(d))
            .flat_map(|f: &Facts| f.reads.iter().map(|(g, _)| *g))
            .collect()
    };
    let mut sets = ReadSets {
        dialogue: reads(&starts),
        parts: Vec::new(),
    };
    for (_, handlers) in &parts {
        let quest_state = reads(handlers)
            .into_iter()
            .filter(|g| !part_globals.contains(g))
            .collect();
        sets.parts.push(quest_state);
    }
    let mut diags = Vec::new();
    let mut sizes = Sizes {
        krate,
        typed,
        memo: BTreeMap::new(),
        uncounted,
    };
    if let Some(d) = sizes.too_big(&sets.dialogue, "the dialogue read set") {
        diags.push(d);
    }
    for ((name, _), set) in parts.iter().zip(&sets.parts) {
        if let Some(d) = sizes.too_big(set, &format!("part `{name}`'s read set")) {
            diags.push(d);
        }
    }
    (sets, diags)
}

/// Static sizes in words.
struct Sizes<'a> {
    krate: &'a Crate,
    typed: &'a Typed,
    memo: BTreeMap<TyId, u32>,
    /// Globals the sum leaves out (`participants`).
    uncounted: BTreeSet<DefId>,
}

impl Sizes<'_> {
    /// `mirror-size` for `set` (`what` names it) above `MIRROR_WORDS_WARN` words.
    fn too_big(&mut self, set: &BTreeSet<DefId>, what: &str) -> Option<Diagnostic> {
        let mut total: u32 = 0;
        let mut largest: Option<(u32, DefId)> = None;
        let counted: Vec<DefId> = set.difference(&self.uncounted).copied().collect();
        for g in &counted {
            let ty = self.typed.globals.get(g).copied().unwrap_or(TyId::ERROR);
            // Its slot, and its value's words.
            let words = 1u32.saturating_add(self.value_words(ty));
            total = total.saturating_add(words);
            if largest.is_none_or(|(w, _)| words > w) {
                largest = Some((words, *g));
            }
        }
        if total <= MIRROR_WORDS_WARN {
            return None;
        }
        let (words, g) = largest?;
        let def = self.krate.def(g)?;
        let file = self
            .krate
            .module(def.module)
            .map_or(String::new(), |m| m.file.clone());
        let name = self.krate.qualified_name(g);
        let mut d = Diagnostic::error(
            "mirror-size",
            format!(
                "{what} is {total} words, more than {MIRROR_WORDS_WARN}: it is mirrored whole on every change, and `{name}` alone is {words}"
            ),
            &file,
            def.span.line,
            def.span.col,
            def.span.len,
        );
        d.severity = Severity::Warning;
        d.notes.push(
            "a `str`, `list` or `dict` counts as unbounded: read a smaller value (a count, a flag) where a condition needs it"
                .to_string(),
        );
        Some(d)
    }

    /// The words a value of `ty` holds beyond its slot: 0 for a scalar, a header and the fields' slots and
    /// values for a struct or an enum's largest variant, `MIRROR_WORDS_WARN` for anything unbounded.
    fn value_words(&mut self, ty: TyId) -> u32 {
        let mut visiting = BTreeSet::new();
        self.words(ty, &mut visiting)
    }

    fn words(&mut self, ty: TyId, visiting: &mut BTreeSet<TyId>) -> u32 {
        if let Some(w) = self.memo.get(&ty) {
            return *w;
        }
        // A type that contains itself is unbounded.
        if !visiting.insert(ty) {
            return MIRROR_WORDS_WARN;
        }
        let fields = |shape: &crate::infer::Shape| shape.tys.clone();
        let w = match self.typed.types.get(ty) {
            Ty::Str | Ty::List(_) | Ty::Dict(_, _) => MIRROR_WORDS_WARN,
            Ty::Option(t) => {
                let inner = self.words(t, visiting);
                2u32.saturating_add(inner)
            }
            Ty::Adt(d) => {
                let shapes: Vec<Vec<TyId>> = self
                    .typed
                    .adts
                    .get(&d)
                    .map(|s| s.iter().map(fields).collect())
                    .unwrap_or_default();
                let mut most = 0u32;
                for tys in shapes {
                    most = most.max(self.fields_words(&tys, visiting));
                }
                // A struct with fields, or an enum with data, is a header and its fields; a field-less
                // variant is its slot alone.
                if most == 0 {
                    0
                } else {
                    1u32.saturating_add(most)
                }
            }
            Ty::HostStruct(i) | Ty::HostEvent(i) => {
                let tys = match self.typed.types.get(ty) {
                    Ty::HostStruct(_) => self.typed.host_structs.get(i as usize).map(fields),
                    _ => self.typed.host_events.get(i as usize).map(fields),
                }
                .unwrap_or_default();
                1u32.saturating_add(self.fields_words(&tys, visiting))
            }
            _ => 0,
        };
        visiting.remove(&ty);
        self.memo.insert(ty, w);
        w
    }

    fn fields_words(&mut self, tys: &[TyId], visiting: &mut BTreeSet<TyId>) -> u32 {
        tys.iter().fold(0u32, |acc, t| {
            acc.saturating_add(1)
                .saturating_add(self.words(*t, visiting))
        })
    }
}
