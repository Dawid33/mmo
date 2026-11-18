//! The compiler's builtins (spec §5.12): `join`, `leave`, `complete`, `fail`, `decline`, `play` as host
//! calls in the namespace `quest` (`quest.joined`, `quest.join_refused`, `quest.left`, `quest.complete`,
//! `quest.fail`, `quest.decline`, `quest.play`, the query `quest.completed_by`), `after` / `cancel` as
//! `After` / `CancelTimer` (a `Timer` is the host struct `Timer { id }`), and `Dialogue` / `Exit` values,
//! strings on the wire: the dialogue's qualified name, and `<dialogue>::<exit>`.

use super::Emit;
use super::expr::BArg;
use super::quest::idx16;
use crate::ast::Span;
use crate::hir::{Arg, DefId, DefKind, DialogueKind, Expr, ExprKind, QuestFn};
use crate::ty::{Ty, TyId};
use quest_vm::asm::SlotRef;
use quest_vm::host::{HostFnKind, QUEST_NAMESPACE};
use quest_vm::types::{TypeDesc, TypeId};
use quest_vm::{Builtin, Op};

/// `NewList`'s flag: the elements are heap kinds.
const LIST_REFS: u16 = 1;

impl Emit<'_, '_, '_> {
    /// A call of the host fn `quest.<name>` with arguments already in words (borrowed: copied into the
    /// call's run): (word, heap kind, VM type). A query's answer goes to `result` (word, VM type).
    pub(super) fn quest_host(
        &mut self,
        name: &str,
        args: &[(SlotRef, bool, TypeId)],
        result: Option<(SlotRef, TypeId)>,
        at: Span,
    ) {
        let host = self.g.host;
        let found = host
            .fns
            .iter()
            .position(|hf| hf.namespace.as_deref() == Some(QUEST_NAMESPACE) && hf.name == name);
        let Some((f, hf)) = found
            .and_then(|f| Some((u16::try_from(f).ok()?, host.fns.get(f)?)))
            .filter(|(_, hf)| hf.params.len() == args.len())
        else {
            return self.not_yet(
                &format!("`{name}` with this host (no `quest.{name}` of this shape)"),
                at,
            );
        };
        let ret = match (hf.kind, result) {
            (HostFnKind::Command, None) => self.type_id(TyId::UNIT, at),
            (HostFnKind::Query, Some((_, t))) => Some(t),
            _ => None,
        };
        let Some(ret) = ret else {
            return self.not_yet(&format!("`quest.{name}` of this kind"), at);
        };
        let params: Vec<TypeId> = args.iter().map(|(_, _, t)| *t).collect();
        let site = self
            .g
            .site(&mut self.fb, f, hf.kind, params, ret, Vec::new());
        let kinds: Vec<bool> = args.iter().map(|(_, h, _)| *h).collect();
        let run = self.slots.run(&kinds);
        for (k, (src, heap, _)) in args.iter().enumerate() {
            let w = self.word(run + k as u32);
            if *heap {
                self.live.push(run + k as u32);
                self.fb.emit(Op::Dup, w, *src, 0u16);
            } else {
                self.fb.emit(Op::Copy, w, *src, 0u16);
            }
        }
        let base = if args.is_empty() {
            self.fb.ret()
        } else {
            self.word(run)
        };
        let c = result.map_or(base, |(w, _)| w);
        self.fb.emit(Op::CallHost, site, base, c);
        for (k, heap) in kinds.iter().enumerate().rev() {
            let i = run + k as u32;
            if *heap {
                let w = self.word(i);
                self.fb.emit(Op::Drop, w, 0u16, 0u16);
            }
            self.free(i);
        }
    }

    /// The VM type of an expression's value (`None`: reported).
    fn tid_of(&mut self, x: &Expr) -> Option<TypeId> {
        let ty = self.target_ty(x);
        self.type_id(ty, x.span)
    }

    /// A call of a compiler builtin (`dialogue_depth` excepted); its value (`()` or a `Timer`) into `dst`.
    pub(super) fn quest_call(&mut self, e: &Expr, f: QuestFn, args: &[Arg], dst: Option<SlotRef>) {
        match f {
            QuestFn::Join => self.join(e, args),
            QuestFn::Leave => self.leave(e, args),
            QuestFn::Complete => self.complete(e, args),
            QuestFn::Fail => self.quest_host("fail", &[], None, e.span),
            QuestFn::Decline => self.quest_host("decline", &[], None, e.span),
            QuestFn::After => return self.after(e, args, dst),
            QuestFn::Cancel => self.cancel(e, args),
            QuestFn::Play => self.play(e, args),
            QuestFn::DialogueDepth => return self.dialogue_depth(e, args, dst),
        }
        if let Some(d) = dst {
            self.fb.emit(Op::LoadSmall, d, 0u16, 0u16);
        }
    }

    /// The participants' global, its VM type.
    fn participants(&mut self, at: Span) -> Option<(SlotRef, TypeId)> {
        let (_, q) = self.quest()?;
        let d = q.generated.participants;
        let g = self.global_slot(d)?;
        let ty = self.g.typed.globals.get(&d).copied()?;
        Some((g, self.type_id(ty, at)?))
    }

    /// `join(p)`: refused while an `exclusive` quest has others, or for a `once_per_player` player who
    /// completed it; else appended when absent (`quest.joined`).
    fn join(&mut self, e: &Expr, args: &[Arg]) {
        let (Some(a), Some((g, _)), Some((_, q))) =
            (args.first(), self.participants(e.span), self.quest())
        else {
            return self.not_yet("this `join`", e.span);
        };
        let Some(pt) = self.tid_of(&a.value) else {
            return;
        };
        let opts = q.repeatable;
        let p = self.val_copy(&a.value);
        let end = self.fb.label();
        let (c, t) = (self.temp(false), self.temp(false));
        let (cw, tw) = (self.word(c), self.word(t));
        self.builtin(
            Builtin::ListContains,
            &[BArg::Word(g, true), BArg::Word(p.slot, true)],
            TyId::BOOL,
            Some(cw),
        );
        let refuse = |s: &mut Self, ok| {
            s.quest_host("join_refused", &[(p.slot, true, pt)], None, e.span);
            s.fb.jump(Op::Jmp, 0u16, end);
            s.fb.bind(ok);
        };
        if opts.is_some_and(|o| o.exclusive) {
            let ok = self.fb.label();
            // Others in it, and not this player: refused.
            self.fb.emit(Op::ListLen, tw, g, 0u16);
            let z = self.temp(false);
            let zw = self.word(z);
            self.fb.emit(Op::LoadSmall, zw, 0u16, 0u16);
            self.fb.emit(Op::LtI, tw, zw, tw);
            self.free(z);
            self.fb.jump(Op::JmpIfNot, tw, ok);
            self.fb.jump(Op::JmpIf, cw, ok);
            refuse(self, ok);
        }
        if opts.is_some_and(|o| o.once_per_player)
            && let Some(bt) = self.type_id(TyId::BOOL, e.span)
        {
            let ok = self.fb.label();
            self.quest_host(
                "completed_by",
                &[(p.slot, true, pt)],
                Some((tw, bt)),
                e.span,
            );
            self.fb.jump(Op::JmpIfNot, tw, ok);
            refuse(self, ok);
        }
        self.fb.jump(Op::JmpIf, cw, end);
        // Appended: the list taken out of the global, pushed onto, put back.
        let run = self.slots.run(&[true, true]);
        self.live.extend([run, run + 1]);
        let (list, item) = (self.word(run), self.word(run + 1));
        self.fb.emit(Op::Move, list, g, 0u16);
        self.fb.emit(Op::Dup, item, p.slot, 0u16);
        self.fb
            .emit(Op::CallBuiltin, Builtin::ListPush as u16, list, list);
        self.fb.emit(Op::Move, g, list, 0u16);
        self.free(run + 1);
        self.free(run);
        self.quest_host("joined", &[(p.slot, true, pt)], None, e.span);
        self.fb.bind(end);
        self.free(t);
        self.free(c);
        self.done(p);
    }

    /// `leave(p)`: the participants without `p` (`quest.left` when it was one).
    fn leave(&mut self, e: &Expr, args: &[Arg]) {
        let (Some(a), Some((g, _))) = (args.first(), self.participants(e.span)) else {
            return self.not_yet("this `leave`", e.span);
        };
        let Some(pt) = self.tid_of(&a.value) else {
            return;
        };
        let p = self.val_copy(&a.value);
        let (i, n, t, found, one) = (
            self.temp(false),
            self.temp(false),
            self.temp(false),
            self.temp(false),
            self.temp(false),
        );
        let (iw, nw, tw, fw, onew) = (
            self.word(i),
            self.word(n),
            self.word(t),
            self.word(found),
            self.word(one),
        );
        // [the list kept, the element]
        let run = self.slots.run(&[true, true]);
        self.live.extend([run, run + 1]);
        let (list, item) = (self.word(run), self.word(run + 1));
        self.fb.emit(Op::NewList, list, LIST_REFS, 0u16);
        self.fb.emit(Op::LoadSmall, iw, 0u16, 0u16);
        self.fb.emit(Op::LoadSmall, fw, 0u16, 0u16);
        self.fb.emit(Op::LoadSmall, onew, 1u16, 0u16);
        self.fb.emit(Op::ListLen, nw, g, 0u16);
        let (top, skip, exit) = (self.fb.label(), self.fb.label(), self.fb.label());
        self.fb.bind(top);
        self.fb.emit(Op::LtI, tw, iw, nw);
        self.fb.jump(Op::JmpIfNot, tw, exit);
        self.fb.emit(Op::GetIndex, item, g, iw);
        self.fb.emit(Op::Eq, tw, item, p.slot);
        self.fb.jump(Op::JmpIf, tw, skip);
        self.fb
            .emit(Op::CallBuiltin, Builtin::ListPush as u16, list, list);
        let next = self.fb.label();
        self.fb.jump(Op::Jmp, 0u16, next);
        self.fb.bind(skip);
        self.fb.emit(Op::Drop, item, 0u16, 0u16);
        self.fb.emit(Op::LoadSmall, fw, 1u16, 0u16);
        self.fb.bind(next);
        self.fb.emit(Op::AddI, iw, iw, onew);
        self.fb.jump(Op::Jmp, 0u16, top);
        self.fb.bind(exit);
        self.fb.emit(Op::Move, g, list, 0u16);
        self.free(run + 1);
        self.free(run);
        let end = self.fb.label();
        self.fb.jump(Op::JmpIfNot, fw, end);
        self.quest_host("left", &[(p.slot, true, pt)], None, e.span);
        self.fb.bind(end);
        for x in [one, found, t, n, i] {
            self.free(x);
        }
        self.done(p);
    }

    /// `complete()` (every participant) / `complete(players)`: `quest.complete`.
    fn complete(&mut self, e: &Expr, args: &[Arg]) {
        match args.first() {
            Some(a) => {
                let Some(t) = self.tid_of(&a.value) else {
                    return;
                };
                let v = self.val(&a.value);
                self.quest_host("complete", &[(v.slot, true, t)], None, e.span);
                self.done(v);
            }
            None => match self.participants(e.span) {
                Some((g, t)) => self.quest_host("complete", &[(g, true, t)], None, e.span),
                None => self.not_yet("this `complete`", e.span),
            },
        }
    }

    /// `after(ticks, Ev(…))`: `After` (a script event only); its `Timer { id }` into `dst`.
    fn after(&mut self, e: &Expr, args: &[Arg], dst: Option<SlotRef>) {
        let (Some(ticks), Some(ev)) = (args.first(), args.get(1)) else {
            return self.not_yet("this `after`", e.span);
        };
        let event = match self.ty_kind(&ev.value) {
            Ty::Adt(d) => self.g.events.get(&d).copied(),
            _ => None,
        };
        let Some(event) = event else {
            return self.not_yet("`after` of a host event", ev.value.span);
        };
        let id_field = self
            .g
            .struct_shape(self.ty_kind(e))
            .and_then(|s| s.names.iter().position(|n| n == "id"))
            .map(idx16);
        // [ticks, the event]
        let run = self.slots.run(&[false, true]);
        self.live.push(run + 1);
        let (tw, vw) = (self.word(run), self.word(run + 1));
        self.eval_into(&ticks.value, tw);
        self.eval_into(&ev.value, vw);
        let id = self.temp(false);
        let iw = self.word(id);
        self.fb.emit(Op::After, iw, tw, event);
        self.fb.emit(Op::Drop, vw, 0u16, 0u16);
        self.free(run + 1);
        self.free(run);
        if let Some(d) = dst {
            match id_field {
                Some(f) => {
                    let shape = self.fb.shape(1, 0);
                    let obj = self.temp(true);
                    let ow = self.word(obj);
                    self.fb.emit(Op::NewObj, ow, shape, 0u16);
                    self.fb.emit(Op::SetField, ow, f, iw);
                    self.fb.emit(Op::Move, d, ow, 0u16);
                    self.free(obj);
                }
                None => self.not_yet("`after` with this host (no `Timer { id }`)", e.span),
            }
        }
        self.free(id);
    }

    /// `cancel(t)`: `CancelTimer` of the timer's id.
    fn cancel(&mut self, e: &Expr, args: &[Arg]) {
        let Some(a) = args.first() else {
            return self.not_yet("this `cancel`", e.span);
        };
        let Some(f) = self.field_index(self.target_kind(&a.value), "id") else {
            return self.not_yet("`cancel` with this host (no `Timer { id }`)", e.span);
        };
        let v = self.val_target(&a.value);
        let id = self.temp(false);
        let iw = self.word(id);
        self.fb.emit(Op::GetField, iw, v.slot, f);
        self.fb.emit(Op::CancelTimer, iw, 0u16, 0u16);
        self.free(id);
        self.done(v);
    }

    /// `play(scene, a=…, …)`: `quest.play(scene, [the cast in the scene's parameter order])`; a scene held
    /// in a variable is `not-yet` (its cast's names are not known).
    fn play(&mut self, e: &Expr, args: &[Arg]) {
        let Some((scene, cast)) = args.split_first() else {
            return self.not_yet("this `play`", e.span);
        };
        // The cast in the scene's parameter order: the scene must be named.
        let order: Vec<&Arg> = match &scene.value.kind {
            ExprKind::Def(d) => match self.def_kind(*d) {
                Some(DefKind::Dialogue(dl)) if dl.kind != DialogueKind::Conversation => dl
                    .cast
                    .iter()
                    .filter_map(|l| {
                        let name = &self.g.krate.local(*l)?.name;
                        cast.iter().find(|a| a.name.as_deref() == Some(name))
                    })
                    .collect(),
                _ => Vec::new(),
            },
            _ => Vec::new(),
        };
        if order.is_empty() {
            return self.not_yet("`play` of a scene that is not named", scene.value.span);
        }
        let (Some(st), Some(first)) = (self.tid_of(&scene.value), order.first()) else {
            return self.not_yet("this `play`", e.span);
        };
        let Some(person) = self.tid_of(&first.value) else {
            return;
        };
        let lt = self.fb.ty(TypeDesc::List(person));
        let s = self.val_copy(&scene.value);
        let run = self.slots.run(&[true, true]);
        self.live.extend([run, run + 1]);
        let (list, item) = (self.word(run), self.word(run + 1));
        self.fb.emit(Op::NewList, list, LIST_REFS, 0u16);
        for a in order {
            self.eval_into(&a.value, item);
            self.fb
                .emit(Op::CallBuiltin, Builtin::ListPush as u16, list, list);
        }
        self.free(run + 1);
        self.quest_host(
            "play",
            &[(s.slot, true, st), (list, true, lt)],
            None,
            e.span,
        );
        self.fb.emit(Op::Drop, list, 0u16, 0u16);
        self.free(run);
        self.done(s);
    }

    /// A dialogue's name as a value: its qualified name.
    pub(super) fn dialogue_value(&mut self, d: DefId, dst: SlotRef) {
        let name = self.g.krate.qualified_name(d);
        let i = self.fb.string(&name);
        self.fb.emit_t(Op::LoadStr, dst, i);
    }

    /// A dialogue's exit `d::name` as a value: `<qualified name>::<exit>`.
    pub(super) fn exit_value(&mut self, d: DefId, exit: u32, dst: SlotRef) {
        let exit = match self.def_kind(d) {
            Some(DefKind::Dialogue(dl)) => dl.exits.get(exit as usize).cloned(),
            _ => None,
        }
        .unwrap_or_default();
        let name = format!("{}::{exit}", self.g.krate.qualified_name(d));
        let i = self.fb.string(&name);
        self.fb.emit_t(Op::LoadStr, dst, i);
    }
}
