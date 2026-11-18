//! The world the stub's queries read: people, creatures, players, sites, places and world facts
//! ([`People`], from a case's `people.txt`), the scripted inputs a [`crate::World`] is driven by
//! ([`Input`], from `script.txt`), and the wire literals both are written in (`Person(3)`, `None`, …).
//!
//! `people.txt`, one entity per line (`#` starts a comment):
//!
//! ```text
//! person 3 name="Hal" occupation=Farmer site=1 alive tags=pet,witness owner=7
//! creature 7 name="Rufus" tags=pet owner=3 pos=1,0,4
//! player 1 name="Ana" skill.trade=3 inv.meat=1
//! site 1 kind=Village
//! place 9 open_ground=30
//! fact act=1
//! fact completed=errantry/x::Q
//! ```
//!
//! Keys: `name`, `occupation` (an `Occupation` variant), `site`, `owner` (ids), `tags` (comma-separated),
//! `kind` (a site's `SiteKind`; anyone else's `kind` string), `open_ground` (blocks), `region`, `pos=x,y,z`,
//! `related` (comma-separated ids), `skill.<name>`, `inv.<kind>`; flags `alive` (the default), `dead`,
//! `ours` (spawned or placed by the quest). An id names one entity.

use std::collections::{BTreeMap, BTreeSet};
use std::hash::Hasher;

use quest_vm::dialogue_run::roll;
use quest_vm::host::QueryError;
use quest_vm::wire::WireValue;
use state_hash::StateHash;

use crate::api::*;
use crate::namespace::error;

mod script;

pub use script::{Input, parse_script, parse_wire};
use script::{ids, int, strip_comment, unquote, words};

/// What an entity is: its handle's struct name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum EntityKind {
    Person,
    Creature,
    Player,
    Site,
    Place,
}

impl EntityKind {
    fn word(self) -> &'static str {
        match self {
            EntityKind::Person => "person",
            EntityKind::Creature => "creature",
            EntityKind::Player => "player",
            EntityKind::Site => "site",
            EntityKind::Place => "place",
        }
    }

    fn handle(self) -> &'static str {
        match self {
            EntityKind::Person => "Person",
            EntityKind::Creature => "Creature",
            EntityKind::Player => "Player",
            EntityKind::Site => "Site",
            EntityKind::Place => "Place",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entity {
    pub kind: EntityKind,
    pub name: String,
    /// An `Occupation` variant's name (`Farmer`).
    pub occupation: Option<String>,
    pub site: Option<i64>,
    pub owner: Option<i64>,
    pub alive: bool,
    pub ours: bool,
    /// A site's `SiteKind` variant; anyone else's `kind` string.
    pub kind_word: Option<String>,
    pub open_ground: Option<i64>,
    pub region: i64,
    pub related: BTreeSet<i64>,
    pub skills: BTreeMap<String, i64>,
    pub inventory: BTreeMap<String, i64>,
    /// `follow(creature, player)`: whom it follows.
    pub following: Option<i64>,
}

impl Entity {
    fn new(kind: EntityKind) -> Entity {
        Entity {
            kind,
            name: String::new(),
            occupation: None,
            site: None,
            owner: None,
            alive: true,
            ours: false,
            kind_word: None,
            open_ground: None,
            region: 0,
            related: BTreeSet::new(),
            skills: BTreeMap::new(),
            inventory: BTreeMap::new(),
            following: None,
        }
    }
}

/// The world the stub's queries read and its commands change.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct People {
    pub entities: BTreeMap<i64, Entity>,
    /// Tags by entity id (an id need not name a known entity: `add_tag` of any player).
    pub tags: BTreeMap<i64, BTreeSet<String>>,
    /// Block positions by entity id.
    pub pos: BTreeMap<i64, [i64; 3]>,
    /// World facts: `act`, `loop`.
    pub facts: BTreeMap<String, i64>,
    /// The definitions completed (`completed(def)`).
    pub completed: BTreeSet<String>,
}

/// The parts of a query's answer that are not the people.
pub(crate) struct QueryCtx<'a> {
    pub tick: u64,
    /// The detour depth `quest.dialogue_depth` answers (0 outside a dialogue).
    pub depth: u32,
    pub completed_by: &'a BTreeSet<i64>,
}

pub(crate) fn field<'a>(v: &'a WireValue, name: &str) -> Option<&'a WireValue> {
    match v {
        WireValue::Struct { fields, .. } | WireValue::Variant { fields, .. } => {
            fields.iter().find(|(n, _)| n == name).map(|(_, v)| v)
        }
        _ => None,
    }
}

pub(crate) fn str_of(v: Option<&WireValue>) -> Option<&str> {
    match v {
        Some(WireValue::Str(s)) => Some(s),
        _ => None,
    }
}

pub(crate) fn int_of(v: Option<&WireValue>) -> Option<i64> {
    match v {
        Some(WireValue::Int(i)) => Some(*i),
        _ => None,
    }
}

/// The `id` of an entity handle (`Player`, `Person`, `Entity`, …), also inside `Some(…)`.
pub fn id_of(v: Option<&WireValue>) -> Option<i64> {
    match v {
        Some(WireValue::Variant { name, fields }) if name == "Some" => {
            id_of(fields.first().map(|(_, v)| v))
        }
        Some(v) => int_of(field(v, "id")),
        None => None,
    }
}

fn handle(name: &str, id: i64) -> WireValue {
    WireValue::Struct {
        name: name.into(),
        fields: vec![("id".into(), WireValue::Int(id))],
    }
}

fn variant(name: String) -> WireValue {
    WireValue::Variant {
        name,
        fields: vec![],
    }
}

impl People {
    /// The handle of entity `id`: `Player { id, name }`, else `<Kind> { id }`; `None` for an unknown id.
    pub fn handle(&self, id: i64) -> Option<WireValue> {
        let e = self.entities.get(&id)?;
        Some(match e.kind {
            EntityKind::Player => WireValue::Struct {
                name: "Player".into(),
                fields: vec![
                    ("id".into(), WireValue::Int(id)),
                    ("name".into(), WireValue::Str(e.name.clone())),
                ],
            },
            k => handle(k.handle(), id),
        })
    }

    /// The name an entity is printed by: its `name`, else `#<id>`.
    pub fn name_of(&self, id: i64) -> String {
        match self.entities.get(&id) {
            Some(e) if !e.name.is_empty() => e.name.clone(),
            _ => format!("#{id}"),
        }
    }

    /// Whether `place(…, at)` of this entity finds no room: its `open_ground` is 0.
    pub fn no_room(&self, id: i64) -> bool {
        self.entities
            .get(&id)
            .is_some_and(|e| e.open_ground == Some(0))
    }

    pub fn has_tag(&self, id: i64, tag: &str) -> bool {
        self.tags.get(&id).is_some_and(|t| t.contains(tag))
    }

    pub(crate) fn set_tag(&mut self, id: i64, tag: &str, on: bool) {
        let tags = self.tags.entry(id).or_default();
        if on {
            tags.insert(tag.to_string());
        } else {
            tags.remove(tag);
        }
        if tags.is_empty() {
            self.tags.remove(&id);
        }
    }

    /// Adds `n` (may be negative) of `kind` to an entity's inventory, never below 0. Unknown ids keep
    /// nothing.
    pub(crate) fn add_items(&mut self, id: i64, kind: &str, n: i64) {
        if let Some(e) = self.entities.get_mut(&id) {
            let slot = e.inventory.entry(kind.to_string()).or_insert(0);
            *slot = slot.saturating_add(n).max(0);
            if *slot == 0 {
                e.inventory.remove(kind);
            }
        }
    }

    /// Blocks apart along the axis they differ most on; `None` unless both have a position.
    fn distance(&self, a: i64, b: i64) -> Option<i64> {
        let (pa, pb) = self.pos.get(&a).zip(self.pos.get(&b))?;
        (0..3)
            .map(|i| pa[i].abs_diff(pb[i]))
            .max()
            .map(|d| i64::try_from(d).unwrap_or(i64::MAX))
    }

    /// `near(a, b, regions=r, blocks=n)`: with `blocks > 0` both have positions at most `n` blocks apart;
    /// else both are known and their regions at most `r` apart.
    fn near(&self, a: i64, b: i64, regions: i64, blocks: i64) -> bool {
        if blocks > 0 {
            return self.distance(a, b).is_some_and(|d| d <= blocks);
        }
        match (self.entities.get(&a), self.entities.get(&b)) {
            (Some(x), Some(y)) => x.region.abs_diff(y.region) <= regions.unsigned_abs(),
            _ => false,
        }
    }

    /// Answers the stub API's query `f` (the ids of `crate::api`); `None` for a fn it does not answer
    /// here (the chest, the namespace, `distance_to`'s fallback are the host's).
    pub(crate) fn query(
        &self,
        f: u16,
        args: &[WireValue],
        ctx: &QueryCtx<'_>,
    ) -> Option<Result<WireValue, QueryError>> {
        let bad = || QueryError::Failed(format!("bad arguments to host fn {f}"));
        let id = |k: usize| id_of(args.get(k)).ok_or_else(bad);
        let text = |k: usize| str_of(args.get(k)).ok_or_else(bad);
        let ent = |k: usize| -> Result<&Entity, QueryError> {
            let i = id(k)?;
            self.entities
                .get(&i)
                .ok_or_else(|| QueryError::Failed(format!("no entity {i}")))
        };
        let b = WireValue::Bool;
        let n = WireValue::Int;
        // `Ok(None)`: not a query the people answer.
        let answer = (|| -> Result<Option<WireValue>, QueryError> {
            Ok(Some(match f {
                F_HAS_TAG | F_PERSON_HAS_TAG | F_CREATURE_HAS_TAG => {
                    b(self.has_tag(id(0)?, text(1)?))
                }
                F_ROLL => {
                    let (salt, sides) = int_of(args.first())
                        .zip(int_of(args.get(1)))
                        .ok_or_else(bad)?;
                    if sides <= 0 {
                        return Ok(Some(n(0)));
                    }
                    // `sides > 0`: the remainder is below `i64::MAX`.
                    n((roll(ctx.tick, salt as u64, 0) % sides as u64) as i64)
                }
                F_ACT | F_LOOP => {
                    let key = if f == F_ACT { "act" } else { "loop" };
                    n(self.facts.get(key).copied().unwrap_or(0))
                }
                F_COMPLETED => b(self.completed.contains(text(0)?)),
                F_NEAR => {
                    let (r, bl) = int_of(args.get(2))
                        .zip(int_of(args.get(3)))
                        .ok_or_else(bad)?;
                    b(self.near(id(0)?, id(1)?, r, bl))
                }
                F_PERSON_ALIVE | F_CREATURE_ALIVE => b(ent(0)?.alive),
                F_PERSON_OCCUPATION | F_CREATURE_OCCUPATION => variant(format!(
                    "Occupation::{}",
                    ent(0)?.occupation.as_deref().unwrap_or("None")
                )),
                // The API's `site` and `owner` are not optional: an entity without one is refused (a
                // condition counts the fault as false), never given a made-up `Site(0)`.
                F_PERSON_SITE | F_CREATURE_SITE => {
                    let i = id(0)?;
                    let site = ent(0)?
                        .site
                        .ok_or_else(|| QueryError::Failed(format!("{i} has no site")))?;
                    handle("Site", site)
                }
                F_PERSON_OWNER | F_CREATURE_OWNER => {
                    let i = id(0)?;
                    let owner = ent(0)?
                        .owner
                        .ok_or_else(|| QueryError::Failed(format!("{i} has no owner")))?;
                    handle("Person", owner)
                }
                F_PERSON_KIND | F_CREATURE_KIND => {
                    let e = ent(0)?;
                    WireValue::Str(e.kind_word.clone().unwrap_or_else(|| e.kind.word().into()))
                }
                F_PERSON_NAME | F_CREATURE_NAME => WireValue::Str(ent(0)?.name.clone()),
                F_SITE_KIND => {
                    let e = ent(0)?;
                    match (&e.kind, &e.kind_word) {
                        (EntityKind::Site, Some(k)) => variant(format!("SiteKind::{k}")),
                        _ => return Err(QueryError::Failed(format!("{} is no site", id(0)?))),
                    }
                }
                F_PLACE_OPEN_GROUND => {
                    let want = int_of(args.get(1)).ok_or_else(bad)?;
                    b(ent(0)?.open_ground.is_none_or(|g| g >= want))
                }
                F_PLAYER_SKILL => n(ent(0)?.skills.get(text(1)?).copied().unwrap_or(0)),
                F_PLAYER_INVENTORY_COUNT => {
                    n(ent(0)?.inventory.get(text(1)?).copied().unwrap_or(0))
                }
                F_OURS => b(self.entities.get(&id(0)?).is_some_and(|e| e.ours)),
                F_PREFAB => WireValue::Struct {
                    name: "Prefab".into(),
                    fields: vec![("name".into(), WireValue::Str(text(0)?.into()))],
                },
                F_QUEST_COMPLETED_BY => b(ctx.completed_by.contains(&id(0)?)),
                F_QUEST_DIALOGUE_DEPTH => n(i64::from(ctx.depth)),
                F_PERSON_DISTANCE_TO => match self.distance(id(0)?, id(1)?) {
                    Some(d) => n(d),
                    None => return Err(QueryError::Raised(error("OutOfReach", vec![]))),
                },
                F_RELATED => {
                    let (x, y) = (id(0)?, id(1)?);
                    let rel = |a: i64, c: i64| {
                        self.entities
                            .get(&a)
                            .is_some_and(|e| e.owner == Some(c) || e.related.contains(&c))
                    };
                    b(rel(x, y) || rel(y, x))
                }
                _ => return Ok(None),
            }))
        })();
        answer.transpose()
    }
}

impl StateHash for People {
    fn state_hash<H: Hasher>(&self, h: &mut H) {
        let count = |h: &mut H, n: usize| (n as u64).state_hash(h);
        count(h, self.entities.len());
        for (id, e) in &self.entities {
            id.state_hash(h);
            (e.kind as u8).state_hash(h);
            e.name.state_hash(h);
            e.occupation.state_hash(h);
            e.site.state_hash(h);
            e.owner.state_hash(h);
            e.alive.state_hash(h);
            e.ours.state_hash(h);
            e.kind_word.state_hash(h);
            e.open_ground.state_hash(h);
            e.region.state_hash(h);
            e.related.state_hash(h);
            e.skills.state_hash(h);
            e.inventory.state_hash(h);
            e.following.state_hash(h);
        }
        self.tags.state_hash(h);
        count(h, self.pos.len());
        for (id, p) in &self.pos {
            id.state_hash(h);
            p.state_hash(h);
        }
        self.facts.state_hash(h);
        self.completed.state_hash(h);
    }
}

/// Parses a `people.txt` (module docs). An error names its line (1-based).
pub fn parse_people(text: &str) -> Result<People, String> {
    let mut people = People::default();
    let api = stub_api();
    let variants = |name: &str| -> Vec<String> {
        api.enums
            .iter()
            .find(|e| e.name == name)
            .map(|e| e.variants.clone())
            .unwrap_or_default()
    };
    let (occupations, site_kinds) = (variants("Occupation"), variants("SiteKind"));
    for (k, raw) in text.lines().enumerate() {
        let at = |e: String| format!("people line {}: {e}", k + 1);
        let line = strip_comment(raw);
        let ws = words(line).ok_or_else(|| at("an unclosed quote or bracket".into()))?;
        let Some((&first, rest)) = ws.split_first() else {
            continue;
        };
        if first == "fact" {
            for w in rest {
                let (key, value) = w
                    .split_once('=')
                    .ok_or_else(|| at(format!("`{w}` is not key=value")))?;
                match key {
                    "completed" => {
                        people.completed.insert(unquote(value).map_err(at)?);
                    }
                    "act" | "loop" => {
                        people
                            .facts
                            .insert(key.to_string(), int(value).map_err(at)?);
                    }
                    _ => return Err(at(format!("unknown fact `{key}`"))),
                }
            }
            continue;
        }
        let kind = match first {
            "person" => EntityKind::Person,
            "creature" => EntityKind::Creature,
            "player" => EntityKind::Player,
            "site" => EntityKind::Site,
            "place" => EntityKind::Place,
            other => return Err(at(format!("unknown line kind `{other}`"))),
        };
        let (&id, attrs) = rest
            .split_first()
            .ok_or_else(|| at("an id is missing".into()))?;
        let id = int(id).map_err(at)?;
        if people.entities.contains_key(&id) {
            return Err(at(format!("id {id} is used twice")));
        }
        let mut e = Entity::new(kind);
        for w in attrs {
            let Some((key, value)) = w.split_once('=') else {
                match *w {
                    "alive" => e.alive = true,
                    "dead" => e.alive = false,
                    "ours" => e.ours = true,
                    _ => return Err(at(format!("unknown flag `{w}`"))),
                }
                continue;
            };
            let value = unquote(value).map_err(at)?;
            match key {
                "name" => e.name = value,
                "occupation" if !occupations.contains(&value) => {
                    return Err(at(format!(
                        "`{value}` is no occupation ({})",
                        occupations.join(", ")
                    )));
                }
                "occupation" => e.occupation = Some(value),
                "site" => e.site = Some(int(&value).map_err(at)?),
                "owner" => e.owner = Some(int(&value).map_err(at)?),
                "kind" if kind == EntityKind::Site && !site_kinds.contains(&value) => {
                    return Err(at(format!(
                        "`{value}` is no site kind ({})",
                        site_kinds.join(", ")
                    )));
                }
                "kind" => e.kind_word = Some(value),
                "open_ground" => e.open_ground = Some(int(&value).map_err(at)?),
                "region" => e.region = int(&value).map_err(at)?,
                "related" => e.related = ids(&value).map_err(at)?,
                "tags" => {
                    for t in value.split(',').filter(|t| !t.is_empty()) {
                        people.set_tag(id, t, true);
                    }
                }
                "pos" => {
                    let p: Vec<i64> = value
                        .split(',')
                        .map(int)
                        .collect::<Result<_, _>>()
                        .map_err(at)?;
                    let p: [i64; 3] = p
                        .try_into()
                        .map_err(|_| at(format!("`{value}` is not x,y,z")))?;
                    people.pos.insert(id, p);
                }
                _ => match key.split_once('.') {
                    Some(("skill", s)) => {
                        e.skills.insert(s.to_string(), int(&value).map_err(at)?);
                    }
                    Some(("inv", s)) => {
                        e.inventory.insert(s.to_string(), int(&value).map_err(at)?);
                    }
                    _ => return Err(at(format!("unknown key `{key}`"))),
                },
            }
        }
        people.entities.insert(id, e);
    }
    Ok(people)
}
