//! The scripted inputs a [`crate::World`] is driven by (`script.txt`, or the CLI), the wire literals they and
//! the roles are written in, and the line-level text helpers `people.txt` shares.

use super::*;

// ---- text ---------------------------------------------------------------------------------------------

/// Splits a line into words at whitespace outside quotes and brackets; `None` on an unclosed one.
pub(super) fn words(line: &str) -> Option<Vec<&str>> {
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

/// A line without its comment (a `#` outside quotes; `\"` inside quotes does not close them).
pub(super) fn strip_comment(line: &str) -> &str {
    let (mut quoted, mut escaped) = (false, false);
    for (i, c) in line.char_indices() {
        match c {
            _ if escaped => escaped = false,
            '\\' if quoted => escaped = true,
            '"' => quoted = !quoted,
            '#' if !quoted => return &line[..i],
            _ => {}
        }
    }
    line
}

/// A quoted string's contents (`\"` and `\\` unescaped), or the word itself.
pub(super) fn unquote(s: &str) -> Result<String, String> {
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

pub(super) fn int(s: &str) -> Result<i64, String> {
    s.parse::<i64>()
        .map_err(|_| format!("`{s}` is not an integer"))
}

pub(super) fn ids(s: &str) -> Result<BTreeSet<i64>, String> {
    s.split(',').filter(|x| !x.is_empty()).map(int).collect()
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
