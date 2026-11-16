//! Exhaustiveness and reachability of `match` arms (spec §5.6, §5.7; plan decision 17): the usefulness
//! algorithm over a pattern matrix (Maranget, "Warnings for pattern matching", 2007), with witnesses.
//!
//! The patterns arrive typed and simplified ([`Pat`]): a binding is [`Pat::Wild`]; a constructor carries its
//! [`Key`] and exactly as many sub-patterns as it has fields (omitted fields are wild); a pattern whose
//! mistake was already reported is wild (no cascade), and one that can never match (an error that cannot
//! occur there) is the empty alternative `Or([])`.
//!
//! A type's constructors come from a [`Space`]: `bool` {true, false}, `()` one, an enum its variants,
//! `Option` {Some, None}, a fallible result {Ok, Err}, a caught error one per name of its set, a struct or an
//! event one. Every other type (`int`, `float`, `str`, collections, an unknown or already-wrong type) has
//! infinitely many: only a wildcard or a binding covers it, and a literal pattern is a constructor of its
//! own that never completes the type.
//!
//! **Recursive types terminate**: a type's constructors are asked for only when a pattern in that column
//! names a constructor, and specialising by one adds exactly that constructor's fields, so the search goes
//! only as deep as the patterns do. A type no pattern constrains is never unfolded.
//!
//! **Guards** never count toward coverage: a guarded arm is left out of the matrix the later arms and the
//! exhaustiveness check see. It is still checked for reachability itself (it may match).
//!
//! **A bound on the work**: usefulness is exponential in the worst case. Each search step costs one unit
//! per row it looks at, and the recursion depth is bounded; past [`WORK_MAX`] units or [`DEPTH_MAX`]
//! levels the answer is "unknown", which the checker reads quietly: the match is accepted as exhaustive
//! and no arm is called unreachable. Both bounds are fixed numbers, so the outcome is deterministic.

use crate::ty::TyId;

/// The work units one `match` may spend (see the module doc).
pub(crate) const WORK_MAX: u32 = 200_000;

/// The deepest the search recurses (see the module doc).
pub(crate) const DEPTH_MAX: u32 = 512;

/// A constructor, as patterns and types name it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Key {
    Bool(bool),
    Int(i64),
    /// A float literal, by its bits.
    Float(u64),
    Str(String),
    /// An enum's variant, by index.
    Variant(u32),
    /// The only constructor of a struct, an event or `()`.
    Single,
    Some,
    None,
    Ok,
    Err,
    /// An error name, in a caught error's set.
    Error(String),
}

/// A typed, simplified pattern.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Pat {
    Wild,
    /// A constructor and one sub-pattern per field.
    Ctor(Key, Vec<Pat>),
    /// Alternatives; empty: matches nothing.
    Or(Vec<Pat>),
}

/// One constructor of a type: its key, its name as source writes it (`Stage::Done`, `Some`, `ChestEmpty`,
/// `true`), and its fields' types.
#[derive(Debug, Clone)]
pub(crate) struct CtorSig {
    pub(crate) key: Key,
    pub(crate) name: String,
    pub(crate) fields: Vec<TyId>,
}

/// What the search knows about types.
pub(crate) trait Space {
    /// The constructors of `ty` in declaration order; `None` when it has infinitely many (or is unknown).
    fn ctors(&mut self, ty: TyId) -> Option<Vec<CtorSig>>;
}

/// One arm of a `match`.
#[derive(Debug, Clone)]
pub(crate) struct Arm {
    pub(crate) pat: Pat,
    pub(crate) guarded: bool,
}

/// What [`check`] found.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Report {
    /// A value no arm matches, printed as a pattern (`Stage::Done`, `Err(ChestEmpty)`, `_`).
    pub(crate) missing: Option<String>,
    /// The arms (by index) that can never match because earlier arms cover them.
    pub(crate) unreachable: Vec<usize>,
}

/// Checks the arms of a `match` on a value of type `ty`.
pub(crate) fn check(space: &mut dyn Space, ty: TyId, arms: &[Arm]) -> Report {
    let mut search = Search { space, work: 0 };
    let mut report = Report::default();
    let mut rows: Vec<Vec<&Pat>> = Vec::new();
    for (i, arm) in arms.iter().enumerate() {
        if let Ok(None) = search.useful(&rows, &[&arm.pat], &[ty], 0) {
            report.unreachable.push(i);
        }
        if !arm.guarded {
            rows.push(vec![&arm.pat]);
        }
    }
    if let Ok(Some(witness)) = search.useful(&rows, &[&WILD], &[ty], 0) {
        report.missing = witness.first().map(Witness::print);
    }
    report
}

static WILD: Pat = Pat::Wild;

/// A value no row matches, as a pattern.
#[derive(Debug, Clone)]
enum Witness {
    Wild,
    Ctor(String, Vec<Witness>),
}

impl Witness {
    fn print(&self) -> String {
        let mut out = String::new();
        let mut stack: Vec<Result<&Witness, &'static str>> = vec![Ok(self)];
        while let Some(item) = stack.pop() {
            match item {
                Err(text) => out.push_str(text),
                Ok(Witness::Wild) => out.push('_'),
                Ok(Witness::Ctor(name, args)) => {
                    out.push_str(name);
                    if args.is_empty() {
                        continue;
                    }
                    out.push('(');
                    stack.push(Err(")"));
                    for (i, a) in args.iter().enumerate().rev() {
                        stack.push(Ok(a));
                        if i > 0 {
                            stack.push(Err(", "));
                        }
                    }
                }
            }
        }
        out
    }
}

/// The search ran out of work or depth: the answer is unknown.
struct Over;

struct Search<'s> {
    space: &'s mut dyn Space,
    work: u32,
}

impl Search<'_> {
    /// Whether the vector `q` (of types `tys`) matches a value no row matches; if so, such a value (one
    /// witness per column of `q`).
    fn useful(
        &mut self,
        rows: &[Vec<&Pat>],
        q: &[&Pat],
        tys: &[TyId],
        depth: u32,
    ) -> Result<Option<Vec<Witness>>, Over> {
        let cost = u32::try_from(rows.len())
            .unwrap_or(u32::MAX)
            .saturating_add(1);
        self.work = self.work.saturating_add(cost);
        if self.work > WORK_MAX || depth > DEPTH_MAX {
            return Err(Over);
        }
        let Some((head, rest)) = q.split_first() else {
            return Ok(rows.is_empty().then(Vec::new));
        };
        let ty = tys.first().copied().unwrap_or(TyId::ERROR);
        let rest_tys = tys.get(1..).unwrap_or(&[]);
        let rows = expand_or(rows);
        match head {
            Pat::Or(alts) => {
                for a in alts {
                    let mut q2: Vec<&Pat> = vec![a];
                    q2.extend_from_slice(rest);
                    if let Some(w) = self.useful(&rows, &q2, tys, depth + 1)? {
                        return Ok(Some(w));
                    }
                }
                Ok(None)
            }
            Pat::Ctor(key, args) => {
                let (name, fields) = self.sig_of(ty, key, args.len());
                let rows2 = specialize(&rows, key, args.len());
                let mut q2: Vec<&Pat> = args.iter().collect();
                q2.extend_from_slice(rest);
                let mut tys2 = fields;
                tys2.extend_from_slice(rest_tys);
                let found = self.useful(&rows2, &q2, &tys2, depth + 1)?;
                Ok(found.map(|w| wrap(name, args.len(), w)))
            }
            Pat::Wild => {
                let heads: Vec<&Key> = rows
                    .iter()
                    .filter_map(|r| match r.first() {
                        Some(Pat::Ctor(k, _)) => Some(k),
                        _ => None,
                    })
                    .collect();
                let sigs = if heads.is_empty() {
                    None
                } else {
                    self.space.ctors(ty)
                };
                let complete = sigs
                    .as_ref()
                    .is_some_and(|s| s.iter().all(|c| heads.contains(&&c.key)));
                if complete {
                    for sig in sigs.unwrap_or_default() {
                        let arity = sig.fields.len();
                        let rows2 = specialize(&rows, &sig.key, arity);
                        let mut q2: Vec<&Pat> = vec![&WILD; arity];
                        q2.extend_from_slice(rest);
                        let mut tys2 = sig.fields;
                        tys2.extend_from_slice(rest_tys);
                        if let Some(w) = self.useful(&rows2, &q2, &tys2, depth + 1)? {
                            return Ok(Some(wrap(sig.name, arity, w)));
                        }
                    }
                    return Ok(None);
                }
                let rows2: Vec<Vec<&Pat>> = rows
                    .iter()
                    .filter(|r| matches!(r.first(), Some(Pat::Wild)))
                    .map(|r| r.get(1..).unwrap_or(&[]).to_vec())
                    .collect();
                let Some(mut w) = self.useful(&rows2, rest, rest_tys, depth + 1)? else {
                    return Ok(None);
                };
                // A constructor no row names, when there are finitely many; else any value.
                let first_missing = sigs
                    .into_iter()
                    .flatten()
                    .find(|c| !heads.contains(&&c.key));
                let head = match first_missing {
                    Some(c) => Witness::Ctor(c.name, vec![Witness::Wild; c.fields.len()]),
                    None => Witness::Wild,
                };
                w.insert(0, head);
                Ok(Some(w))
            }
        }
    }

    /// The name and field types of the constructor `key` of `ty`; a literal (or a constructor of a type
    /// the space does not list) has no fields the search needs to type.
    fn sig_of(&mut self, ty: TyId, key: &Key, arity: usize) -> (String, Vec<TyId>) {
        let found = self
            .space
            .ctors(ty)
            .into_iter()
            .flatten()
            .find(|c| c.key == *key);
        match found {
            Some(c) if c.fields.len() == arity => (c.name, c.fields),
            _ => (String::from("_"), vec![TyId::ERROR; arity]),
        }
    }
}

/// The rows with every alternative at their head made a row of its own (nested ones too).
fn expand_or<'p>(rows: &[Vec<&'p Pat>]) -> Vec<Vec<&'p Pat>> {
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let mut stack: Vec<&'p Pat> = row.first().copied().into_iter().collect();
        let mut heads: Vec<&'p Pat> = Vec::new();
        while let Some(p) = stack.pop() {
            match p {
                Pat::Or(alts) => stack.extend(alts.iter().rev()),
                other => heads.push(other),
            }
        }
        let tail = row.get(1..).unwrap_or(&[]);
        for h in heads {
            let mut r = Vec::with_capacity(row.len());
            r.push(h);
            r.extend_from_slice(tail);
            out.push(r);
        }
    }
    out
}

/// The rows that match the constructor `key` (of `arity` fields) at their head, with its fields in place of
/// the head. The heads are not alternatives (`expand_or`).
fn specialize<'p>(rows: &[Vec<&'p Pat>], key: &Key, arity: usize) -> Vec<Vec<&'p Pat>> {
    let mut out = Vec::new();
    for row in rows {
        let tail = row.get(1..).unwrap_or(&[]);
        let mut r: Vec<&'p Pat> = Vec::with_capacity(arity + tail.len());
        match row.first() {
            Some(Pat::Ctor(k, args)) if k == key => {
                r.extend(args.iter());
                // Typed patterns always have their constructor's arity; a shorter one is padded.
                for _ in args.len()..arity {
                    r.push(&WILD);
                }
                r.truncate(arity);
            }
            Some(Pat::Wild) => r.extend(std::iter::repeat_n(&WILD, arity)),
            _ => continue,
        }
        r.extend_from_slice(tail);
        out.push(r);
    }
    out
}

/// A witness for `[name(args…), rest…]` from one for `[args…, rest…]`.
fn wrap(name: String, arity: usize, mut w: Vec<Witness>) -> Vec<Witness> {
    let n = arity.min(w.len());
    let args: Vec<Witness> = w.drain(..n).collect();
    w.insert(0, Witness::Ctor(name, args));
    w
}
