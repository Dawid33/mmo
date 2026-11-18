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
        let answer = (|| -> Result<WireValue, QueryError> {
            Ok(match f {
                F_HAS_TAG | F_PERSON_HAS_TAG | F_CREATURE_HAS_TAG => {
                    b(self.has_tag(id(0)?, text(1)?))
                }
                F_ROLL => {
                    let (salt, sides) = int_of(args.first())
                        .zip(int_of(args.get(1)))
                        .ok_or_else(bad)?;
                    if sides <= 0 {
                        return Ok(n(0));
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
                F_PERSON_SITE | F_CREATURE_SITE => handle("Site", ent(0)?.site.unwrap_or(0)),
                F_PERSON_OWNER | F_CREATURE_OWNER => handle("Person", ent(0)?.owner.unwrap_or(0)),
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
                _ => return Err(QueryError::Failed(String::new())),
            })
        })();
        match answer {
            Err(QueryError::Failed(m)) if m.is_empty() => None,
            other => Some(other),
        }
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

// ---- text ---------------------------------------------------------------------------------------------

/// Splits a line into words at whitespace outside quotes and brackets; `None` on an unclosed one.
fn words(line: &str) -> Option<Vec<&str>> {
    let mut out = Vec::new();
    let (mut depth, mut quoted, mut start) = (0i32, false, None);
    let mut escaped = false;
    for (i, c) in line.char_indices() {
        if quoted {
            match c {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => quoted = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => quoted = true,
            '(' | '[' => depth += 1,
            ')' | ']' => depth -= 1,
            c if c.is_whitespace() && depth == 0 => {
                if let Some(s) = start.take() {
                    out.push(&line[s..i]);
                }
                continue;
            }
            _ => {}
        }
        if start.is_none() {
            start = Some(i);
        }
    }
    if quoted || depth != 0 {
        return None;
    }
    if let Some(s) = start {
        out.push(&line[s..]);
    }
    Some(out)
}

/// A line without its comment (a `#` outside quotes).
fn strip_comment(line: &str) -> &str {
    let mut quoted = false;
    for (i, c) in line.char_indices() {
        match c {
            '"' => quoted = !quoted,
            '#' if !quoted => return &line[..i],
            _ => {}
        }
    }
    line
}

/// A quoted string's contents (`\"` and `\\` unescaped), or the word itself.
fn unquote(s: &str) -> Result<String, String> {
    let Some(inner) = s.strip_prefix('"') else {
        return Ok(s.to_string());
    };
    let inner = inner
        .strip_suffix('"')
        .ok_or_else(|| format!("unclosed string {s}"))?;
    let mut out = String::new();
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            out.push(chars.next().ok_or("a string ends in `\\`")?);
        } else {
            out.push(c);
        }
    }
    Ok(out)
}

fn int(s: &str) -> Result<i64, String> {
    s.parse::<i64>()
        .map_err(|_| format!("`{s}` is not an integer"))
}

fn ids(s: &str) -> Result<BTreeSet<i64>, String> {
    s.split(',').filter(|x| !x.is_empty()).map(int).collect()
}

/// Parses a `people.txt` (module docs). An error names its line (1-based).
pub fn parse_people(text: &str) -> Result<People, String> {
    let mut people = People::default();
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
                "occupation" => e.occupation = Some(value),
                "site" => e.site = Some(int(&value).map_err(at)?),
                "owner" => e.owner = Some(int(&value).map_err(at)?),
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

/// One scripted input (`script.txt`, or the CLI), at a tick.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Input {
    /// `talk <player> <npc>`.
    Talk { player: i64, npc: i64 },
    /// `pick <player> <n>`: option `n` (1-based) of the offer the player has.
    Pick { player: i64, option: u32 },
    /// `leave <player>`: walks away from the conversation.
    Leave { player: i64 },
    /// `done <player>`: the end acknowledgement.
    Done { player: i64 },
    /// `event <Name> <field>=<wire>… [@<part>]`: a host or script event to the home, or to the part.
    Event {
        name: String,
        /// Each field's wire literal, parsed when delivered (a `Player(1)` takes its name then).
        fields: Vec<(String, String)>,
        part: Option<String>,
    },
    /// `near <player> <entity>`: `PlayerNear` to the parts anchored on the entity.
    Near { player: i64, entity: i64 },
    /// `kill <victim> [by <player>]`.
    Kill { victim: i64, by: Option<i64> },
    /// `lose <entity>`: the role bound to it stops resolving.
    Lose { entity: i64 },
    /// `ambient <npc> <npc>…`: the ambient schedule casts from these idle people.
    Ambient { npcs: Vec<i64> },
    /// `edit <dir>`: the script is edited (the world's edit hook compiles it).
    Edit { dir: String },
}

/// Parses a `script.txt`: `<tick> <verb> <args>` per line (`#` comments). Ticks may not go down. An error
/// names its line (1-based).
pub fn parse_script(text: &str) -> Result<Vec<(u64, Input)>, String> {
    let mut out: Vec<(u64, Input)> = Vec::new();
    for (k, raw) in text.lines().enumerate() {
        let at = |e: String| format!("script line {}: {e}", k + 1);
        let ws =
            words(strip_comment(raw)).ok_or_else(|| at("an unclosed quote or bracket".into()))?;
        let Some((&tick, rest)) = ws.split_first() else {
            continue;
        };
        let tick: u64 = tick
            .parse()
            .map_err(|_| at(format!("`{tick}` is not a tick")))?;
        if out.last().is_some_and(|(t, _)| *t > tick) {
            return Err(at(format!("tick {tick} is earlier than the line before")));
        }
        let (&verb, args) = rest
            .split_first()
            .ok_or_else(|| at("an input is missing".into()))?;
        let arg = |i: usize| -> Result<i64, String> {
            int(args
                .get(i)
                .ok_or_else(|| at(format!("`{verb}` needs more arguments")))?)
            .map_err(at)
        };
        let exact = |n: usize| -> Result<(), String> {
            if args.len() == n {
                Ok(())
            } else {
                Err(at(format!("`{verb}` takes {n} arguments")))
            }
        };
        let input = match verb {
            "talk" => {
                exact(2)?;
                Input::Talk {
                    player: arg(0)?,
                    npc: arg(1)?,
                }
            }
            "pick" => {
                exact(2)?;
                let option = u32::try_from(arg(1)?)
                    .ok()
                    .filter(|n| *n >= 1)
                    .ok_or_else(|| at("an option is numbered from 1".into()))?;
                Input::Pick {
                    player: arg(0)?,
                    option,
                }
            }
            "leave" | "done" => {
                exact(1)?;
                let player = arg(0)?;
                if verb == "leave" {
                    Input::Leave { player }
                } else {
                    Input::Done { player }
                }
            }
            "near" => {
                exact(2)?;
                Input::Near {
                    player: arg(0)?,
                    entity: arg(1)?,
                }
            }
            "kill" => {
                let by = match args.len() {
                    1 => None,
                    3 if args[1] == "by" => Some(arg(2)?),
                    _ => return Err(at("`kill <victim> [by <player>]`".into())),
                };
                Input::Kill {
                    victim: arg(0)?,
                    by,
                }
            }
            "lose" => {
                exact(1)?;
                Input::Lose { entity: arg(0)? }
            }
            "ambient" => {
                if args.is_empty() {
                    return Err(at("`ambient` names the idle people".into()));
                }
                Input::Ambient {
                    npcs: (0..args.len()).map(arg).collect::<Result<_, _>>()?,
                }
            }
            "edit" => {
                exact(1)?;
                Input::Edit {
                    dir: unquote(args[0]).map_err(at)?,
                }
            }
            "event" => {
                let (&name, mut rest) = args
                    .split_first()
                    .ok_or_else(|| at("`event` names the event".into()))?;
                let mut part = None;
                if let Some((last, init)) = rest.split_last()
                    && let Some(p) = last.strip_prefix('@')
                {
                    part = Some(p.to_string());
                    rest = init;
                }
                let mut fields = Vec::new();
                for w in rest {
                    let (f, v) = w
                        .split_once('=')
                        .ok_or_else(|| at(format!("`{w}` is not field=value")))?;
                    parse_wire(v, &People::default()).map_err(at)?;
                    fields.push((f.to_string(), v.to_string()));
                }
                Input::Event {
                    name: name.to_string(),
                    fields,
                    part,
                }
            }
            other => return Err(at(format!("unknown input `{other}`"))),
        };
        out.push((tick, input));
    }
    Ok(out)
}

/// Most nesting a wire literal may have.
const WIRE_DEPTH_MAX: u32 = 32;

/// Parses a wire literal: an int, a float with a `.`, a `"string"`, `true` / `false`, `None`, `Some(v)`,
/// `[a, b]`, `Enum::Variant` (a host enum keeps its qualified name, `Occupation::Smith`; another is the
/// bare variant), an entity handle `Person(3)` (`Player(1)` takes its name from `people`), and a struct
/// `Name(field=v, …)`.
pub fn parse_wire(text: &str, people: &People) -> Result<WireValue, String> {
    let mut p = WireParser {
        s: text.as_bytes(),
        at: 0,
        people,
    };
    let v = p.value(0)?;
    p.ws();
    if p.at != p.s.len() {
        return Err(format!("`{text}`: unexpected text after the value"));
    }
    Ok(v)
}

struct WireParser<'a> {
    s: &'a [u8],
    at: usize,
    people: &'a People,
}

impl WireParser<'_> {
    fn ws(&mut self) {
        while self.s.get(self.at).is_some_and(u8::is_ascii_whitespace) {
            self.at += 1;
        }
    }

    fn eat(&mut self, c: u8) -> bool {
        self.ws();
        if self.s.get(self.at) == Some(&c) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    fn word(&mut self) -> String {
        self.ws();
        let start = self.at;
        while self
            .s
            .get(self.at)
            .is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b':' | b'-' | b'.'))
        {
            self.at += 1;
        }
        String::from_utf8_lossy(&self.s[start..self.at]).into_owned()
    }

    fn value(&mut self, depth: u32) -> Result<WireValue, String> {
        if depth > WIRE_DEPTH_MAX {
            return Err("a wire literal nests too deeply".into());
        }
        self.ws();
        match self.s.get(self.at) {
            Some(b'"') => self.string(),
            Some(b'[') => {
                self.at += 1;
                let items = self.list(b']', depth)?;
                Ok(WireValue::List(items))
            }
            Some(_) => {
                let w = self.word();
                if w.is_empty() {
                    return Err("a value is missing".into());
                }
                self.after_word(w, depth)
            }
            None => Err("a value is missing".into()),
        }
    }

    fn string(&mut self) -> Result<WireValue, String> {
        let start = self.at;
        self.at += 1;
        let mut escaped = false;
        while let Some(&c) = self.s.get(self.at) {
            self.at += 1;
            match c {
                _ if escaped => escaped = false,
                b'\\' => escaped = true,
                b'"' => {
                    let text = String::from_utf8_lossy(&self.s[start..self.at]);
                    return unquote(&text).map(WireValue::Str);
                }
                _ => {}
            }
        }
        Err("an unclosed string".into())
    }

    fn list(&mut self, close: u8, depth: u32) -> Result<Vec<WireValue>, String> {
        let mut items = Vec::new();
        if self.eat(close) {
            return Ok(items);
        }
        loop {
            items.push(self.value(depth + 1)?);
            if self.eat(close) {
                return Ok(items);
            }
            if !self.eat(b',') {
                return Err("expected `,` or a closing bracket".into());
            }
        }
    }

    fn after_word(&mut self, w: String, depth: u32) -> Result<WireValue, String> {
        match w.as_str() {
            "true" => return Ok(WireValue::Bool(true)),
            "false" => return Ok(WireValue::Bool(false)),
            "None" => return Ok(variant("None".into())),
            _ => {}
        }
        if w.starts_with(|c: char| c.is_ascii_digit() || c == '-') {
            if w.contains('.') {
                return w
                    .parse::<f64>()
                    .map(WireValue::Float)
                    .map_err(|_| format!("`{w}` is not a number"));
            }
            return int(&w).map(WireValue::Int);
        }
        if !self.eat(b'(') {
            return Ok(match w.split_once("::") {
                Some((e, v)) if !stub_api().enums.iter().any(|x| x.name == e) => {
                    variant(v.to_string())
                }
                _ => variant(w),
            });
        }
        // `Name(…)`: positional (an entity handle, `Some`, `Prefab`) or named fields.
        let save = self.at;
        let named = {
            let f = self.word();
            !f.is_empty() && self.eat(b'=')
        };
        self.at = save;
        if !named {
            let args = self.list(b')', depth)?;
            return match (w.as_str(), args.as_slice()) {
                ("Some", [v]) => Ok(WireValue::Variant {
                    name: "Some".into(),
                    fields: vec![("value".into(), v.clone())],
                }),
                ("Player", [WireValue::Int(id)]) => Ok(WireValue::Struct {
                    name: "Player".into(),
                    fields: vec![
                        ("id".into(), WireValue::Int(*id)),
                        (
                            "name".into(),
                            WireValue::Str(
                                self.people
                                    .entities
                                    .get(id)
                                    .map(|e| e.name.clone())
                                    .unwrap_or_default(),
                            ),
                        ),
                    ],
                }),
                ("Prefab", [WireValue::Str(n)]) => Ok(WireValue::Struct {
                    name: "Prefab".into(),
                    fields: vec![("name".into(), WireValue::Str(n.clone()))],
                }),
                (_, [WireValue::Int(id)]) => Ok(handle(&w, *id)),
                (_, []) => Ok(WireValue::Struct {
                    name: w,
                    fields: vec![],
                }),
                _ => Err(format!("`{w}(…)`: give its fields by name")),
            };
        }
        let mut fields = Vec::new();
        loop {
            let f = self.word();
            if f.is_empty() || !self.eat(b'=') {
                return Err(format!("`{w}(…)`: expected `field=value`"));
            }
            fields.push((f, self.value(depth + 1)?));
            if self.eat(b')') {
                break;
            }
            if !self.eat(b',') {
                return Err(format!("`{w}(…)`: expected `,` or `)`"));
            }
        }
        Ok(WireValue::Struct { name: w, fields })
    }
}
