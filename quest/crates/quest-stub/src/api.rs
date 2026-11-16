//! The stub's host-API description (design D9, version 1) and the host-fn ids it implies: a host fn id is the
//! index into [`stub_api`]`().fns`.

use quest_vm::host::{HostApi, HostError, HostFn, HostFnKind, HostStruct, HostTy};

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
    use HostTy::{Bool, Event, Infer, Int, Str, Struct, Unit, Value};
    let player = || Struct("Player".into());
    let item = || Struct("Item".into());
    let key = || ("key", Str);
    HostApi {
        version: 1,
        structs: vec![
            event("Player", &[("id", Int), ("name", Str)]),
            event("Item", &[("kind", Str), ("count", Int)]),
        ],
        events: vec![
            "PlayerClicked".into(),
            "PlayerEntered".into(),
            "ItemDelivered".into(),
        ],
        fns: vec![
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
            f(
                Some("Player"),
                true,
                "has_tag",
                &[("tag", Str)],
                Bool,
                &[],
                Query,
            ),
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
        ],
        errors: vec![
            err("ChestEmpty", &[]),
            err("NotEnough", &[("have", Int)]),
            err("NotFound", &[]),
            err("WrongType", &[]),
            err("Changed", &[("current", Infer)]),
            err("Timeout", &[]),
            err("Lost", &[]),
            err("NamespaceGone", &[]),
        ],
    }
}

fn err(name: &str, fields: &[(&str, HostTy)]) -> HostError {
    HostError {
        name: name.to_string(),
        fields: fields.iter().map(|(n, t)| p(n, t.clone())).collect(),
    }
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
