//! The stub's host-API description (design D9, version 4) and the host-fn ids it implies: a host fn id is the
//! index into [`stub_api`]`().fns`.
//!
//! Version 2 adds the quest host API of spec §9.1 ("Assumed host API" of the quest examples): the entity
//! handles and types, the enums `Occupation` and `SiteKind`, the fact queries (marked `fact` for the query
//! subset) and other pure queries, the commands, the events, and in the namespace `quest` the host side of
//! the quest builtins. The types and events a quest generates or the compiler owns (`Entered`, `RoleLost`,
//! `Talked`, `DialogueEnded`, `SceneEnded`, `EndReason`, `SceneEnd`, `Exit`, `Stage`, `Role`) are not here.
//! A `Dialogue` parameter is `Value` until the compiler has its own `Dialogue` type.
//!
//! Version 3 adds a fallible query, `Person.distance_to(other) -> Int raises OutOfReach`.
//!
//! Version 4 (the query subset's fact table): `Player.has_tag` is a fact like every other `has_tag`, `kind`
//! is a fact, and the relation `related(a, b)` is added (a fact).

use quest_vm::host::{
    HostApi, HostEnum, HostError, HostFn, HostFnKind, HostStruct, HostTy, PROMISE_ERRORS,
    QUEST_NAMESPACE,
};

pub const F_SAY: u16 = 0;
pub const F_LOG: u16 = 1;
pub const F_GIVE: u16 = 2;
pub const F_ADD_TAG: u16 = 3;
pub const F_REMOVE_TAG: u16 = 4;
pub const F_CHEST_PUT: u16 = 5;
pub const F_EMIT: u16 = 6;
pub const F_HAS_TAG: u16 = 7;
pub const F_ROLL: u16 = 8;
pub const F_CHEST_COUNT: u16 = 9;
pub const F_CHEST_TAKE: u16 = 10;
pub const F_WORLD_GET: u16 = 11;
pub const F_WORLD_SET: u16 = 12;
pub const F_WORLD_ADD: u16 = 13;
pub const F_WORLD_CAS: u16 = 14;
pub const F_WORLD_DELETE: u16 = 15;
pub const F_PLACE: u16 = 42;
pub const F_PERSON_DISTANCE_TO: u16 = 56;

fn p(name: &str, ty: HostTy) -> (String, HostTy) {
    (name.to_string(), ty)
}

fn event(name: &str, fields: &[(&str, HostTy)]) -> HostStruct {
    HostStruct {
        name: name.to_string(),
        fields: fields.iter().map(|(n, t)| p(n, t.clone())).collect(),
    }
}

/// A host fn. `owner` is the namespace (`chest`, `world`), or the receiver type when `method`.
fn f(
    owner: Option<&str>,
    method: bool,
    name: &str,
    params: &[(&str, HostTy)],
    ret: HostTy,
    raises: &[&str],
    kind: HostFnKind,
) -> HostFn {
    let owner = owner.map(str::to_string);
    HostFn {
        namespace: if method { None } else { owner.clone() },
        receiver: if method { owner } else { None },
        name: name.to_string(),
        params: params.iter().map(|(n, t)| p(n, t.clone())).collect(),
        ret,
        raises: raises.iter().map(|r| r.to_string()).collect(),
        kind,
        property: false,
        fact: false,
        defaults: Vec::new(),
    }
}

/// `f` marked as a fact: a query the coordinator can evaluate.
fn fact(mut f: HostFn) -> HostFn {
    f.fact = true;
    f
}

/// A property `owner.name` (read without parentheses), a fact when `is_fact`.
fn property(owner: &str, name: &str, ret: HostTy, is_fact: bool) -> HostFn {
    let mut p = f(Some(owner), true, name, &[], ret, &[], HostFnKind::Query);
    p.property = true;
    p.fact = is_fact;
    p
}

/// A free command (no namespace, no receiver) that cannot fail.
fn command(name: &str, params: &[(&str, HostTy)]) -> HostFn {
    f(
        None,
        false,
        name,
        params,
        HostTy::Unit,
        &[],
        HostFnKind::Command,
    )
}

/// A command in the `quest` namespace.
fn quest_command(name: &str, params: &[(&str, HostTy)]) -> HostFn {
    f(
        Some(QUEST_NAMESPACE),
        false,
        name,
        params,
        HostTy::Unit,
        &[],
        HostFnKind::Command,
    )
}

fn host_enum(name: &str, variants: &[&str]) -> HostEnum {
    HostEnum {
        name: name.to_string(),
        variants: variants.iter().map(|v| v.to_string()).collect(),
    }
}

/// The error names every `world.*` promise may raise besides its own.
const WORLD_COMMON: [&str; 3] = ["Timeout", "Lost", "NamespaceGone"];

fn world(name: &str, params: &[(&str, HostTy)], ret: HostTy, own: &[&str]) -> HostFn {
    let raises: Vec<&str> = own.iter().chain(WORLD_COMMON.iter()).copied().collect();
    f(
        Some("world"),
        false,
        name,
        params,
        ret,
        &raises,
        HostFnKind::Promise,
    )
}

/// The stub's host API. A method's receiver is not in `params`; the stub expects it as the first argument.
pub fn stub_api() -> HostApi {
    use HostFnKind::{Command, Promise, Query};
    use HostTy::{Bool, Enum, Event, Infer, Int, List, Str, Struct, Unit, Value};
    let player = || Struct("Player".into());
    let item = || Struct("Item".into());
    let person = || Struct("Person".into());
    let creature = || Struct("Creature".into());
    let site = || Struct("Site".into());
    let place = || Struct("Place".into());
    let prefab = || Struct("Prefab".into());
    let key = || ("key", Str);
    let mut fns = vec![
        f(
            None,
            false,
            "say",
            &[("p", player()), ("text", Str)],
            Unit,
            &[],
            Command,
        ),
        f(None, false, "log", &[("text", Str)], Unit, &[], Command),
        f(
            None,
            false,
            "give",
            &[("p", player()), ("item", item())],
            Unit,
            &[],
            Command,
        ),
        f(
            Some("Player"),
            true,
            "add_tag",
            &[("tag", Str)],
            Unit,
            &[],
            Command,
        ),
        f(
            Some("Player"),
            true,
            "remove_tag",
            &[("tag", Str)],
            Unit,
            &[],
            Command,
        ),
        f(
            Some("chest"),
            false,
            "put",
            &[("item", item())],
            Unit,
            &[],
            Command,
        ),
        f(None, false, "emit", &[("e", Event)], Unit, &[], Command),
        // A fact since version 4: tags are world data the coordinator holds.
        fact(f(
            Some("Player"),
            true,
            "has_tag",
            &[("tag", Str)],
            Bool,
            &[],
            Query,
        )),
        f(
            None,
            false,
            "roll",
            &[("salt", Int), ("sides", Int)],
            Int,
            &[],
            Query,
        ),
        f(
            Some("chest"),
            false,
            "count",
            &[("kind", Str)],
            Int,
            &[],
            Query,
        ),
        f(
            Some("chest"),
            false,
            "take",
            &[("kind", Str), ("n", Int)],
            item(),
            &["ChestEmpty", "NotEnough"],
            Promise,
        ),
        world("get", &[key()], Infer, &["NotFound", "WrongType"]),
        world("set", &[key(), ("v", Value)], Unit, &[]),
        world("add", &[key(), ("n", Int)], Int, &["NotFound", "WrongType"]),
        world(
            "compare_and_set",
            &[key(), ("expected", Value), ("new", Value)],
            Unit,
            &["NotFound", "Changed"],
        ),
        world("delete", &[key()], Unit, &["NotFound"]),
    ];
    // Version 2: facts.
    let mut near = fact(f(
        None,
        false,
        "near",
        &[
            ("a", Value),
            ("b", Value),
            ("regions", Int),
            ("blocks", Int),
        ],
        Bool,
        &[],
        Query,
    ));
    near.defaults = vec![("regions".to_string(), 0), ("blocks".to_string(), 0)];
    fns.extend([
        fact(f(None, false, "act", &[], Int, &[], Query)),
        fact(f(None, false, "loop", &[], Int, &[], Query)),
        fact(f(
            None,
            false,
            "completed",
            &[("def", Str)],
            Bool,
            &[],
            Query,
        )),
        near,
    ]);
    // The entity handles' properties and tags.
    for owner in ["Person", "Creature"] {
        fns.extend([
            property(owner, "alive", Bool, true),
            property(owner, "occupation", Enum("Occupation".into()), true),
            property(owner, "site", site(), true),
            property(owner, "owner", person(), true),
            property(owner, "kind", Str, true),
            property(owner, "name", Str, false),
            fact(f(
                Some(owner),
                true,
                "has_tag",
                &[("tag", Str)],
                Bool,
                &[],
                Query,
            )),
        ]);
    }
    fns.extend([
        fact(f(
            None,
            false,
            "site_kind",
            &[("s", site())],
            Enum("SiteKind".into()),
            &[],
            Query,
        )),
        fact(f(
            Some("Place"),
            true,
            "open_ground",
            &[("blocks", Int)],
            Bool,
            &[],
            Query,
        )),
        // Other pure queries.
        f(
            Some("Player"),
            true,
            "skill",
            &[("name", Str)],
            Int,
            &[],
            Query,
        ),
        f(
            Some("Player"),
            true,
            "inventory_count",
            &[("kind", Str)],
            Int,
            &[],
            Query,
        ),
        f(None, false, "ours", &[("e", Value)], Bool, &[], Query),
        f(
            None,
            false,
            "prefab",
            &[("name", Str)],
            prefab(),
            &[],
            Query,
        ),
        // Commands.
        command("take", &[("p", player()), ("kind", Str), ("n", Int)]),
        command("follow", &[("c", creature()), ("p", player())]),
        f(
            None,
            false,
            "place",
            &[("prefab", prefab()), ("at", place())],
            Unit,
            &["NoRoom"],
            Command,
        ),
        command("tag", &[("e", Value), ("t", Str)]),
        command("attach_dialogue", &[("npc", person()), ("d", Value)]),
        command("detach_dialogue", &[("npc", person()), ("d", Value)]),
        command("bark", &[("npc", person()), ("d", Value)]),
        // The host side of the quest builtins.
        quest_command("joined", &[("p", player())]),
        quest_command("left", &[("p", player())]),
        quest_command("join_refused", &[("p", player())]),
        quest_command("complete", &[("ps", List(Box::new(player())))]),
        quest_command("fail", &[]),
        quest_command("decline", &[]),
        quest_command("play", &[("scene", Value), ("cast", List(Box::new(Value)))]),
        fact(f(
            Some(QUEST_NAMESPACE),
            false,
            "completed_by",
            &[("p", player())],
            Bool,
            &[],
            Query,
        )),
        f(
            Some(QUEST_NAMESPACE),
            false,
            "dialogue_depth",
            &[],
            Int,
            &[],
            Query,
        ),
    ]);
    // Version 3: a fallible query.
    fns.push(f(
        Some("Person"),
        true,
        "distance_to",
        &[("other", Value)],
        Int,
        &["OutOfReach"],
        Query,
    ));
    // Version 4: `has_tag` is a fact on every receiver, `kind` a fact, and the relation `related(a, b)`.
    fns.push(fact(f(
        None,
        false,
        "related",
        &[("a", Value), ("b", Value)],
        Bool,
        &[],
        Query,
    )));
    HostApi {
        version: 4,
        structs: vec![
            event("Player", &[("id", Int), ("name", Str)]),
            event("Item", &[("kind", Str), ("count", Int)]),
            event("Person", &[("id", Int)]),
            event("Creature", &[("id", Int)]),
            event("Site", &[("id", Int)]),
            event("Place", &[("id", Int)]),
            event("Prefab", &[("name", Str)]),
            event("Timer", &[("id", Int)]),
        ],
        enums: vec![
            host_enum(
                "Occupation",
                &[
                    "Farmer",
                    "Miller",
                    "Smith",
                    "Guard",
                    "Elder",
                    "Miner",
                    "Merchant",
                    "Innkeeper",
                    "None",
                ],
            ),
            host_enum("SiteKind", &["Village", "Town", "Mine", "Camp"]),
        ],
        events: vec![
            event("PlayerClicked", &[("player", player())]),
            event("PlayerEntered", &[("player", player())]),
            event("ItemDelivered", &[("player", player()), ("item", item())]),
            event("PlayerNear", &[("player", player())]),
            event(
                "ItemGiven",
                &[("player", player()), ("to", Value), ("item", item())],
            ),
            event(
                "Killed",
                &[
                    ("victim", Value),
                    ("by", HostTy::Option(Box::new(player()))),
                ],
            ),
            event("PlayerDefeated", &[("player", player())]),
            event("BlockUsed", &[("player", player()), ("tag", Str)]),
            event("Setup", &[]),
        ],
        fns,
        errors: vec![
            err("ChestEmpty", &[]),
            err("NotEnough", &[("have", Int)]),
            err("NotFound", &[]),
            err("WrongType", &[]),
            err("Changed", &[("current", Infer)]),
            err("Timeout", &[]),
            err("Lost", &[]),
            err("NamespaceGone", &[]),
            err("NoRoom", &[]),
            err("OutOfReach", &[]),
        ],
    }
}

fn err(name: &str, fields: &[(&str, HostTy)]) -> HostError {
    HostError {
        name: name.to_string(),
        fields: fields.iter().map(|(n, t)| p(n, t.clone())).collect(),
    }
}

/// Whether the host fn `id` can fail with the error `name`: it declares it, or it is a promise and `name` is
/// one of the base promise errors (`quest_vm::host::PROMISE_ERRORS`).
pub fn op_can_raise(id: u16, name: &str) -> bool {
    let api = stub_api();
    let Some(hf) = api.fns.get(usize::from(id)) else {
        return false;
    };
    hf.raises.iter().any(|r| r == name)
        || (hf.kind == HostFnKind::Promise && PROMISE_ERRORS.contains(&name))
}

/// `owner.name` for a host fn id (`world.get`, `Player.add_tag`, `emit`): what a [`crate::FailureRule`]
/// names. `None` for an id the API does not have.
pub fn op_name(id: u16) -> Option<String> {
    let api = stub_api();
    let hf = api.fns.get(usize::from(id))?;
    Some(match hf.namespace.as_ref().or(hf.receiver.as_ref()) {
        Some(owner) => format!("{owner}.{}", hf.name),
        None => hf.name.clone(),
    })
}
