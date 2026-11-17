//! The resolved dialogues, scenes and quests (spec §5.11–5.12).
//!
//! A dialogue is data: its nodes and steps mirror the syntax tree, with every expression replaced by the
//! [`DefId`] of a **synthesised pure `fn`** ([`super::Fn::pure`]): conditions, substitutions, `emit`
//! arguments, `for` targets, filters, a scene's `where`, a role's `where`, `requires` and stage transitions.
//! A synthesised fn is an ordinary [`super::DefKind::Fn`], named `<owner>$<site>#<n>` (`farmer_talk$cond#0`,
//! `LostDog::plea$subst#1`, `LostDog$where#0`): `<owner>` is the dialogue's (or, for a quest's own sites, the
//! quest's) qualified name, `<site>` the [`PureSite`]'s word, `n` the source order among that owner's sites
//! of that kind. Its parameters are the cast (`p, npc` for a conversation, the scene's parameters), none for
//! `requires`, a transition or an objective's line, and for a role's `where` the roles declared before it
//! then the role itself.
//!
//! A quest's names (`Stage`, `Role`, `stage`, `participants`, each objective and role, `Entered`,
//! `RoleLost`) are generated items of the root module whose [`super::Def::parent`] is the quest, so their
//! qualified names are `Q::stage`, `Q::Search.find_dog`, `Q::role.farmer`; a part's own items are
//! `Q::<part>::<name>`. These names are migration keys.

use super::{DefId, EventRef, LocalId, Type};
use crate::ast::Span;

/// Where a synthesised pure fn stands (spec §5.11): what it computes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PureSite {
    /// A `when`, an option's or a topic's `if`: `bool`.
    Cond,
    /// The value `once per expr` keys its record by.
    Per,
    /// An f-string hole of a line: `str` (the hole wrapped in `str(…)`).
    Subst,
    /// An `emit` argument: the field's type.
    EmitArg,
    /// A `for` target, an objective's `at`: a `Person` / `Option[Person]` (an entity).
    Target,
    /// A line's speaker that is not one of the cast (a role, a global).
    Speaker,
    /// `for npc where pred`: `bool`.
    Filter,
    /// A scene's `where`: `bool`.
    CastCond,
    /// A role's `where` (the role's index): `bool`.
    Where(RoleIdx),
    /// A quest's `requires`: `bool`.
    Requires,
    /// A stage's completion transition, to the stage given: `bool`.
    Transition(StageIdx),
}

impl PureSite {
    /// The word in a synthesised fn's name.
    pub fn word(self) -> &'static str {
        match self {
            PureSite::Cond => "cond",
            PureSite::Per => "per",
            PureSite::Subst => "subst",
            PureSite::EmitArg => "emit_arg",
            PureSite::Target => "target",
            PureSite::Speaker => "speaker",
            PureSite::Filter => "filter",
            PureSite::CastCond => "cast_cond",
            PureSite::Where(_) => "where",
            PureSite::Requires => "requires",
            PureSite::Transition(_) => "transition",
        }
    }
}

/// A stage: its index in [`Quest::stages`] (source order; the `Stage` enum's variant index).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct StageIdx(pub u32);

/// A role: its index in [`Quest::roles`] (source order; the `Role` enum's variant index).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct RoleIdx(pub u32);

/// Which item a [`Dialogue`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogueKind {
    /// `dialogue name(p: Player, npc: Person)`.
    Conversation,
    /// `dialogue name(npc: Person)`: a one-actor scene.
    Bark,
    /// `scene name(a: Person, …)`.
    Scene,
}

/// `@pick(…)`: how node and line groups pick a member (spec §5.11). The default is `BestLeastRecent`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pick {
    First,
    Best,
    BestLeastRecent,
    RandomBestLeastRecent,
}

/// A line id: `@farmer.start`, or the provisional `<dialogue>.<n>` of a line written without one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineId {
    pub path: String,
    pub provisional: bool,
}

/// A piece of a line's source text.
#[derive(Debug, Clone, PartialEq)]
pub enum LinePart {
    Text(String),
    /// An f-string hole: its substitution fn ([`PureSite::Subst`]) and the hole's source position.
    Subst(DefId, Span),
}

/// A line's text: its id and its source text, holes replaced by their substitution fns.
#[derive(Debug, Clone, PartialEq)]
pub struct LineRef {
    pub id: LineId,
    pub parts: Vec<LinePart>,
    pub span: Span,
}

impl LineRef {
    /// The substitution fns, in source order.
    pub fn subst(&self) -> Vec<DefId> {
        self.parts
            .iter()
            .filter_map(|p| match p {
                LinePart::Subst(d, _) => Some(*d),
                LinePart::Text(_) => None,
            })
            .collect()
    }
}

/// `topic "text" [@id] [if cond] [priority n]`.
#[derive(Debug, Clone, PartialEq)]
pub struct Topic {
    pub text: LineRef,
    pub cond: Option<DefId>,
    pub priority: i64,
}

/// `once [per expr]`: the record is per instance, or per value of the `per` fn ([`PureSite::Per`]).
#[derive(Debug, Clone, PartialEq)]
pub struct Once {
    pub per: Option<DefId>,
}

/// The condition of a node, a line or a `when` step: `once [per e] [if e]`, or a plain condition.
#[derive(Debug, Clone, PartialEq)]
pub struct Cond {
    pub once: Option<Once>,
    pub guard: Option<DefId>,
}

/// `node name [when cond]:` + steps. A body without nodes is one implicit node named `""`.
#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    pub name: String,
    /// The node group: nodes sharing a name share it (groups numbered by first appearance).
    pub group: u32,
    pub cond: Option<Cond>,
    pub steps: Vec<Step>,
    pub span: Span,
}

/// Who says a line.
#[derive(Debug, Clone, PartialEq)]
pub enum Speaker {
    /// A member of the cast, by parameter position (`p` is 0 in a conversation, `npc` 1).
    Cast(u32),
    /// Any other name in scope (a role, a global): its fn ([`PureSite::Speaker`]).
    Fn(DefId),
}

/// `[speaker:] "text" [@id] [(ticks=n)] [when cond]`.
#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    pub speaker: Option<Speaker>,
    pub text: LineRef,
    pub ticks: Option<i64>,
    pub cond: Option<Cond>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    pub kind: StepKind,
    pub span: Span,
}

/// An option of a `choice`; the hidden fallback `default:` has no text.
#[derive(Debug, Clone, PartialEq)]
pub struct ChoiceOption {
    pub text: Option<LineRef>,
    pub guard: Option<DefId>,
    pub once: bool,
    pub default: bool,
    pub steps: Vec<Step>,
    pub span: Span,
}

/// `=> line` and its steps.
#[derive(Debug, Clone, PartialEq)]
pub struct GroupMember {
    pub line: Line,
    pub steps: Vec<Step>,
    pub span: Span,
}

/// An argument of a dialogue's `emit`: by position or by name, its fn ([`PureSite::EmitArg`]).
#[derive(Debug, Clone, PartialEq)]
pub struct EmitArg {
    pub name: Option<String>,
    pub value: DefId,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StepKind {
    Line(Line),
    /// `choice[(timeout=n)]:` + options.
    Choice {
        timeout: Option<i64>,
        options: Vec<ChoiceOption>,
    },
    /// A line group.
    Group(Vec<GroupMember>),
    When(Cond, Vec<Step>),
    Once(Once, Vec<Step>),
    /// `-> node`: the node group.
    Jump(u32),
    /// A scene's last step `-> other_scene`: the scene that follows with the same cast.
    FollowUp(DefId),
    /// `detour name[:` + arms`]`: the callee and its arms by the callee's exit index.
    Detour(DefId, Vec<(u32, Vec<Step>)>),
    Emit(EventRef, Vec<EmitArg>),
    /// `return [exit]`: an index into [`Dialogue::exits`] (0 = `done`).
    Return(u32),
    /// `pause n`.
    Pause(i64),
}

/// A conversation, bark or scene.
#[derive(Debug, Clone, PartialEq)]
pub struct Dialogue {
    pub kind: DialogueKind,
    /// The quest it is declared in.
    pub quest: Option<DefId>,
    /// The stage it is declared in: attached only while that stage is current.
    pub stage: Option<StageIdx>,
    /// The parameters' bindings (`p, npc`, the scene's cast). Each synthesised fn binds its own.
    pub cast: Vec<LocalId>,
    /// `for target` ([`PureSite::Target`]); `None` in the filter form `for npc where pred`.
    pub target: Option<DefId>,
    /// `for npc where pred` ([`PureSite::Filter`]).
    pub filter: Option<DefId>,
    pub topic: Option<Topic>,
    /// `[0]` is the implicit `done`, then the declared exits in order.
    pub exits: Vec<String>,
    pub pick: Pick,
    pub uninterruptible: bool,
    /// A scene's `where` ([`PureSite::CastCond`]).
    pub cast_cond: Option<DefId>,
    pub nodes: Vec<Node>,
    /// Package-level content: a filter dialogue or a scene declared outside any quest (spec §5.11).
    pub stateless: bool,
}

/// `repeatable quest Q(options)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RepeatOpts {
    pub cooldown: i64,
    pub exclusive: bool,
    pub once_per_player: bool,
}

/// `[optional] role name: T [where cond]`.
#[derive(Debug, Clone, PartialEq)]
pub struct RoleDef {
    pub name: String,
    pub ty: Type,
    pub optional: bool,
    /// The `where` ([`PureSite::Where`]).
    pub cond: Option<DefId>,
    /// The read-only global holding it (`Q::role.<name>`; `Option[T]` when optional).
    pub global: DefId,
}

/// `[optional] objective name "text" [@id] [at target] [count n]`.
#[derive(Debug, Clone, PartialEq)]
pub struct ObjectiveDef {
    pub name: String,
    pub text: LineRef,
    /// The marker's target ([`PureSite::Target`]).
    pub at: Option<DefId>,
    pub count: Option<i64>,
    pub optional: bool,
    /// Its state, `Q::<Stage>.<name>` of the built-in struct `Objective`.
    pub global: DefId,
}

/// `stage Name:` and its members.
#[derive(Debug, Clone, PartialEq)]
pub struct StageDef {
    pub name: String,
    pub objectives: Vec<ObjectiveDef>,
    /// `-> Stage when cond`, in source order: the stage and its fn ([`PureSite::Transition`]).
    pub transitions: Vec<(StageIdx, DefId)>,
    pub dialogues: Vec<DefId>,
    pub handlers: Vec<DefId>,
    pub span: Span,
}

/// What a part is anchored to: the role's entity (`on`) or the region it was bound in (`at`).
pub use crate::ast::PartAnchor;

/// `part name on|at role:`.
#[derive(Debug, Clone, PartialEq)]
pub struct PartDef {
    pub name: String,
    pub anchor: PartAnchor,
    pub role: RoleIdx,
    /// Its `let`s (`Q::<part>::<name>`).
    pub globals: Vec<DefId>,
    pub events: Vec<DefId>,
    pub handlers: Vec<DefId>,
    pub span: Span,
}

/// A quest (spec §5.12).
#[derive(Debug, Clone, PartialEq)]
pub struct Quest {
    pub name: String,
    pub repeatable: Option<RepeatOpts>,
    /// `at role`, else the first role (0 also when there is none).
    pub home: RoleIdx,
    pub roles: Vec<RoleDef>,
    /// The `requires` fns, in source order ([`PureSite::Requires`]).
    pub requires: Vec<DefId>,
    pub stages: Vec<StageDef>,
    pub parts: Vec<PartDef>,
    /// The quest's own `let`s (`Q::<name>`).
    pub globals: Vec<DefId>,
    /// The quest's own events.
    pub events: Vec<DefId>,
    /// The handlers of the quest body (any stage).
    pub handlers: Vec<DefId>,
    /// The dialogues and scenes of the quest body (any stage).
    pub dialogues: Vec<DefId>,
    /// The generated items.
    pub generated: QuestItems,
}

/// A quest's generated items.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuestItems {
    /// `enum Stage` (variants: the stages in source order).
    pub stage_enum: DefId,
    /// `enum Role` (variants: the roles, spelled as their names).
    pub role_enum: DefId,
    /// `stage: Stage`.
    pub stage: DefId,
    /// `participants: list[Player]`.
    pub participants: DefId,
    /// `event Entered { stage: Stage }`.
    pub entered: DefId,
    /// `event RoleLost { role: Role }`.
    pub role_lost: DefId,
}

/// The compiler's own items every module sees (after its own items, before the host's): the struct
/// `Objective { state: int, count: int }`, the events `Talked`, `DialogueEnded`, `SceneEnded` and the enums
/// `EndReason`, `SceneEnd`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Builtins {
    pub objective: Option<DefId>,
    pub talked: Option<DefId>,
    pub dialogue_ended: Option<DefId>,
    pub scene_ended: Option<DefId>,
    pub end_reason: Option<DefId>,
    pub scene_end: Option<DefId>,
}

/// The functions only the compiler lowers: quest builtins (`join`, `leave`, `complete`, `fail`, `decline`,
/// inside a quest), timers and scenes (`after`, `cancel`, `play`, anywhere) and the runner's
/// `dialogue_depth()` (inside a dialogue's conditions).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum QuestFn {
    Join,
    Leave,
    Complete,
    Fail,
    Decline,
    After,
    Cancel,
    Play,
    DialogueDepth,
}

impl QuestFn {
    pub fn name(self) -> &'static str {
        match self {
            QuestFn::Join => "join",
            QuestFn::Leave => "leave",
            QuestFn::Complete => "complete",
            QuestFn::Fail => "fail",
            QuestFn::Decline => "decline",
            QuestFn::After => "after",
            QuestFn::Cancel => "cancel",
            QuestFn::Play => "play",
            QuestFn::DialogueDepth => "dialogue_depth",
        }
    }
}

/// The quest builtins a quest's code sees.
pub const QUEST_FNS: [QuestFn; 5] = [
    QuestFn::Join,
    QuestFn::Leave,
    QuestFn::Complete,
    QuestFn::Fail,
    QuestFn::Decline,
];

/// The builtins every module sees.
pub const SCRIPT_FNS: [QuestFn; 3] = [QuestFn::After, QuestFn::Cancel, QuestFn::Play];
