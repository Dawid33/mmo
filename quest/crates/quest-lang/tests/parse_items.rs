//! The parser's statements, items, recovery and the dialogue / quest grammar (plan Task 18).

use quest_lang::parse_file;

/// Parses `text` as `main.qs` and dumps it, failing on any diagnostic.
fn parse_ok(text: &str) -> String {
    let (file, diags) = parse_file("main.qs", text);
    assert!(diags.is_empty(), "{text}\n{diags:#?}");
    file.dump()
}

/// The code and line of each diagnostic.
fn codes(text: &str) -> Vec<(&'static str, u32)> {
    let (_, diags) = parse_file("main.qs", text);
    diags.iter().map(|d| (d.code, d.line)).collect()
}

fn assert_has(dump: &str, part: &str) {
    assert!(dump.contains(part), "missing {part}\nin\n{dump}");
}

// The spec's code blocks, copied in (statements wrapped in `fn main():`).

const S5_1: &str = r#"
fn main():
    let x = 1          # new binding; shadows any earlier x in this or an outer scope
    x = 2              # reassigns the nearest existing x (compile error if none)
    let x = "now a string"   # shadowing may change the type
"#;

const S5_3: &str = r#"
fn main():
    let a = [1, 2]
    let b = a          # copy
    b.append(3)        # a == [1, 2], b == [1, 2, 3]
    let r = &a         # reference
    r.append(9)        # a == [1, 2, 9]
"#;

const S5_4: &str = r#"
fn reward_for(stage):              # types inferred
    if stage == 0:
        return 0
    elif stage < 3:
        return 10 * stage
    else:
        return 50

fn main():
    let total = 0
    for s in range(5):
        total = total + reward_for(s)
    let i = 0
    while i < 10:
        i = i + 1
        if i == 7:
            break
    print(f"total {total}, stopped at {i}")
"#;

const S5_5: &str = r#"
struct Ledger:
    owner: str
    entries: list[int]

impl Ledger:
    fn total(self) -> int:          # self by value (a COW copy — cheap)
        let t = 0
        for e in self.entries:
            t = t + e
        return t

    fn add(&self, n):               # &self: may mutate
        self.entries.append(n)

fn main():
    let l = Ledger(owner="ana", entries=[])
    l.add(5)
"#;

const S5_6: &str = r#"
enum Stage:
    Asked
    Accepted(tick: int)
    Done

fn main():
    match stage:
        case Stage::Asked:
            say(p, "Will you help?")
        case Stage::Accepted(tick) if tick > 100:
            say(p, "Still waiting on you.")
        case Stage::Accepted(_):
            pass
        case Stage::Done:
            say(p, "Thanks again.")
"#;

const S5_7_ITEMS: &str = r#"
error ChestError = ChestEmpty | NotEnough(have: int)
error QuestError = ChestError | QuestClosed | NotYourQuest
"#;

const S5_7_STMTS: &str = r#"
fn main():
    let gold = take_gold(c, 20)?            # propagate (adds the callee's set to ours)
    let gold = take_gold(c, 20) else 0      # fallback value
    let gold = take_gold(c, 20) else return # fallback: diverge
    let gold = take_gold(c, 20) else err:   # fallback block; must yield a value or diverge
        log(f"failed: {err}")
        return

    match take_gold(c, 20):
        case Ok(gold):
            give(p, gold)
        case Err(NotEnough(have)):
            say(p, f"Only {have} left.")
        case Err(ChestEmpty | QuestClosed):
            pass
"#;

const S5_10: &str = r#"
mod rewards
mod npc
use rewards::give_gold
use npc::dialogue::{greet, farewell}
use rewards::give_gold as pay

pub struct Ledger:
    entries: list[int]
"#;

const S7: &str = r#"
@on(PlayerClicked)
fn greet(e):                     # e: PlayerClicked, inferred from the decorator
    say(e.player, "Well met.")

@on(PlayerEntered)
fn warn(e):
    if e.player.has_tag("outlaw"):
        say(e.player, "Begone.")
"#;

const S8_2: &str = r#"
@on(ItemDelivered)
fn claim(e):
    match await world.compare_and_set("wyrm_quest", "open", e.player.id):
        case Ok(_):
            give(e.player, chest.take("gold", 20)?)
        case Err(Changed(winner)):
            say(e.player, "Another claimed it first.")

@on(PlayerClicked)
fn ask(e):
    let slain = await world.get("dragons_slain")?
    let p = world.get(f"renown/{e.player.id}")   # hold a promise, await later
    say(e.player, "Let me check the ledgers…")
    let r = await p else 0
"#;

const S5_11: &str = r#"
let accepted = false
let dog_name = "Rufus"

event Accepted:
    player: Player

@pick(best)
dialogue farmer_talk(p: Player, npc: Person) for farmer:
    node start:
        npc: "Have you seen my dog? He ran off toward the hills."     @farmer.start
        choice:
            "I'll find him." @farmer.accept if not accepted:
                emit Accepted(player=p)
                npc: "Bless you. Bring him back before dark."          @farmer.thanks
            "What does he look like?" @farmer.ask_dog:
                npc: f"Brown, one ear up. He answers to {dog_name}."   @farmer.dog_desc
                -> start
            once "Is that blood on your sleeve?" @farmer.blood:
                detour farmer_secret
            "Goodbye." @farmer.goodbye
        npc: "Safe roads."                                             @farmer.bye

@on(Accepted)
fn accept(e):
    accepted = true
    give(e.player, Item(kind="dog whistle", count=1))

dialogue greeting(p: Player, npc: Person) for innkeeper:
    node hello when p.has_tag("outlaw"):
        npc: "Keep walking."                                           @greet.outlaw
    node hello when once per p:
        npc: "A new face! Welcome."                                    @greet.first
    node hello:
        => npc: "Morning."                                             @greet.a
        => npc: "Weather's turning."                                   @greet.b
        => npc: "You again?"                                           @greet.c when once per p
"#;

const S5_12: &str = r#"
quest LostDog:
    requires act() >= 1
    role farmer: Person where farmer.occupation == Occupation::Farmer and farmer.alive
    role dog: Creature where dog.has_tag("pet") and dog.owner == farmer
    optional role miller: Person where miller.occupation == Occupation::Miller

    let attempts = 0

    event Accepted:
        player: Player
    event DogFound:
        player: Player
    event Nightfall:
        attempt: int

    stage Open:
        dialogue plea(p: Player, npc: Person) for farmer topic f"About {dog.name}" @lostdog.topic:
            npc: f"{dog.name} ran off toward the hills."                @lostdog.plea
            choice:
                "I'll find him." @lostdog.accept:
                    emit Accepted(player=p)
                "Not now." @lostdog.not_now
        @on(Accepted)
        fn accepted(e):
            join(e.player)
            -> Search

    stage Search:
        objective find_dog f"Find {dog.name}" @lostdog.find at dog
        objective feed_dog f"Coax {dog.name} with food" @lostdog.feed at dog
        optional objective ask_miller "Ask at the mill" @lostdog.mill at miller
        -> Returning when find_dog.done and feed_dog.done
        @on(Entered)
        fn count_attempt(e):
            attempts = attempts + 1
        @on(DogFound)
        fn found(e):
            if not find_dog.done:
                find_dog.finish()
                after(9000, Nightfall(attempt=attempts))
        @on(Nightfall)
        fn bolted(e):
            -> Search                       # a failed attempt: re-entering re-opens the objectives

    stage Returning:
        objective bring_home f"Bring {dog.name} home" @lostdog.return at farmer
        @on(DogDelivered)
        fn delivered(e):
            bring_home.finish()
            complete()                      # for every participant

    part dog_watch on dog:
        @on(PlayerNear)
        fn spotted(e):
            if stage == Stage::Search and e.player in participants:
                emit DogFound(player=e.player)
"#;

const EX1: &str = r#"
enum Day:
    Monday
    Tuesday
    Wednesday

enum TimeOfDay:
    Morning
    Evening

let day = Day::Monday              # the current day (in game: a host query)
let time = TimeOfDay::Morning      # the current time of day

dialogue alice_talk(p: Player, npc: Person) for npc_named("Alice"):
    node talk when day == Day::Monday:
        p: "Hey"
        npc: "Hey, it's Monday"
        p: "yep"
        npc: "and I only have a single storylet for Monday regardless of the time of day"
        p: "cool"
        npc: "indeed"
    node talk when day == Day::Tuesday and time == TimeOfDay::Morning:
        p: "Hello"
        npc: "Hello, what a great Tuesday morning it is!"
    node talk when day == Day::Tuesday and time == TimeOfDay::Evening:
        p: "Hello"
        npc: "Ah Tuesday evening, always the best time of Tuesday I say"
    node talk when day == Day::Tuesday and time == TimeOfDay::Evening:
        p: "Hello"
        npc: "Just between you and me I have some plans this evening."
    node talk when day == Day::Wednesday and time == TimeOfDay::Morning:
        p: "Hello"
        npc: "Humpday, am I right?"
    node talk when day == Day::Wednesday and time == TimeOfDay::Morning:
        p: "Hello"
        npc: "Ah Wednesday morning, definitely the most morning part of the day."
    node talk when once if day == Day::Wednesday and time == TimeOfDay::Evening:
        p: "Hello"
        npc: "the week is half over, what a relief."
    node talk when day == Day::Wednesday and time == TimeOfDay::Evening:
        p: "Hello"
        npc: "Probably should go to bed, it is Wednesday evening after all."

dialogue barry_talk(p: Player, npc: Person) for npc_named("Barry"):
    # Three nodes with the same condition: best_least_recent cycles through them.
    node talk when time == TimeOfDay::Morning:
        p: "Hi"
        npc: "Good morning"
    node talk when time == TimeOfDay::Morning:
        p: "Hi"
        npc: "How are you this morning?"
    node talk when time == TimeOfDay::Morning:
        p: "Hi"
        npc: "Top of the morning"
    node talk when time == TimeOfDay::Evening:
        p: "Hi"
        npc: "Good evening"
    node talk when time == TimeOfDay::Evening:
        p: "Hi"
        npc: "What a fine evening it is today"
    node talk when time == TimeOfDay::Evening:
        p: "Hi"
        npc: "Wait, is is it evening or afternoon?"

dialogue liz_talk(p: Player, npc: Person) for npc_named("Liz"):
    node talk when once:           # scores 1, so it wins once; a node with no condition scores 0
        npc: "I have this node which will only happen once, neato right?"
    node talk:
        npc: "Oh hi"
    node talk:
        npc: "What up?"
    node talk:
        npc: "Talk to anyone interesting lately?"

dialogue george_talk(p: Player, npc: Person) for npc_named("George"):
    => npc: "oh hi"
    => npc: "another Monday, I hate Mondays" when day == Day::Monday
    => npc: "I suppose I should make breakfast" when time == TimeOfDay::Morning
    => npc: "Weird how tomorrow is Monday, right?" when day == Day::Wednesday and time == TimeOfDay::Evening:
        p: "did you want the devs to have to write out even more sample content?"
        npc: "it would be nice, yes"
        p: "well they aren'that gonna do that"
        npc: "boo"
    => npc: "I wonder if I am in any other samples"
"#;

const EX2: &str = r#"
let dragon_dead = false
let king_new_advisor = false
let crier = npc_named("Town Crier")

dialogue crier_bark(npc: Person):                   # a bark: no player, no choices
    => npc: "Hear ye, hear ye!"                     @crier.bark.hear_ye
    => npc: "The king has a new advisor!"           @crier.bark.advisor when king_new_advisor
    => npc: "A dragon stalks the village!"          @crier.bark.dragon when not dragon_dead
    => npc: "The fearsome dragon is slain!"         @crier.bark.slain when dragon_dead

dialogue crier_talk(p: Player, npc: Person) for crier:
    node start:
        p: "Hello."                                 @crier.hello
        npc: "Hello, citizen!"                      @crier.hello_back
        -> hub
    node hub:
        choice:
            "What's the latest news?"               @crier.ask_news:
                detour crier_news
            "Goodbye."                              @crier.goodbye:
                npc: "Farewell!"                    @crier.farewell
                return                              # Yarn's <<stop>>
        -> hub

dialogue crier_news(p: Player, npc: Person):
    node news when dragon_dead:
        npc: "The beast that stalked the land of late is slain by a valiant adventurer!"  @crier.news.slain
    node news when not dragon_dead:
        npc: "A fearsome dragon stalks the village from the mountains!"                   @crier.news.dragon
        npc: "Will a noble hero save us from this fell beast?"                            @crier.news.hero

@on(Ambient)
fn call_out(e):
    if e.npc == crier:
        bark(e.npc, crier_bark)                     # a dialogue's name is a Dialogue value
"#;

const EX3: &str = r#"
dialogue timeouts(p: Player, npc: Person) for npc_named("Alice"):
    node start:
        npc: "This sample shows off three different ways of having an option be selected automatically after a fixed amount of time."
        npc: "Which would you like to see?"
        choice:
            "Hidden fallback option?":
                -> fallback
            "Default option?":
                -> default_example
    node fallback:
        npc: "In this example if no option is chosen before time runs out we chose the option with the fallback metadata."
        npc: "This differs from the default example as the fallback option is hidden from the user."
        choice(timeout=300):                    # ticks
            "Option 1?":
                npc: "option 1 was selected"
            "Option 2?":
                npc: "option 2 was selected"
            default:                            # hidden: never offered, runs on timeout
                npc: "The hidden fallback option was selected"
    node default_example:
        npc: "In this example if no option is chosen before time runs out we chose the option with the default metadata."
        npc: "this differs from the fallback example as the default is visible to the user."
        choice(timeout=300):
            default "Option 1?":                # offered, and picked on timeout
                npc: "option 1 was selected"
                npc: "this is the default."
            "Option 2?":
                npc: "option 2 was selected"
"#;

const EX4: &str = r#"
enum ScenarioState:
    NotStarted
    Started
    Complete

enum Room:
    Mansion
    Pub
    Office
    Church

let state = ScenarioState::NotStarted
let room = Room::Mansion
let primary = npc_named("Alice")
let secondary = npc_named("Bob")

event Advance:
    to: ScenarioState

@on(Advance)
fn advance(e):
    state = e.to

dialogue primary_talk(p: Player, npc: Person) for primary:
    # Alice's opening scores once 1 + two comparisons 2 + `and` 1 = 4 and wins over the generic opening (1).
    node talk when once if npc.name == "Alice" and state == ScenarioState::NotStarted:
        npc: f"So it's time to break {secondary.name}."
        p: "oh?"
        npc: "yeah they are real twisted and I will need your help"
        p: "oh cool"
        npc: "agreed, anyways, talk to me again please and we'll discuss strategy."
    node talk when state == ScenarioState::NotStarted:
        npc: "ok you be good cop, I'll be bad"
        p: "yep"
        emit Advance(to=ScenarioState::Started)
    node talk when state == ScenarioState::Started:
        npc: "you are in real trouble now"
        secondary: "Oh am I now!?"
        => p: "it's weird we're in a mansion for this right?" when room == Room::Mansion
        => p: "anyone fancy a pint?" when room == Room::Pub
        => p: "this feels like the right place for this" when room == Room::Office
        => p: "I reckon the symbolism here is confusing" when room == Room::Church
        npc: "quiet for a second please"
        p: "want to go see Les Mis?"
        secondary: "huh?"
        npc: "Yeah, what?"
        p: "anyone fancy an icecream?, I could go pick us some up."
        secondary: "I confess, just stop being so weird."
        emit Advance(to=ScenarioState::Complete)
    node talk when state == ScenarioState::Complete:
        npc: "look I am not sure what you were doing but wow did it ever work."
        p: "thanks"

dialogue secondary_talk(p: Player, npc: Person) for secondary:
    node talk when state == ScenarioState::Complete:
        => npc: "I promise I'll be good, please don't Henry the Train me."
        => npc: "I'm done hiding the corpses of my masters victims." when room == Room::Mansion
        => npc: "I'm all Jesused up now, I swear." when room == Room::Church
        => npc: f"I promise I'll never eat {primary.name}'s lunch again." when room == Room::Office
        => npc: "I promise I'll no more cheating at darts." when room == Room::Pub
    node talk:
        npc: "Why are you talking to me?"
"#;

const EX5: &str = r#"
event Sell:
    player: Player
    kind: str

fn has_room(p: Player, kind: str) -> bool:      # pure: usable in conditions
    return p.inventory_count(kind) < 10

@on(Sell)
fn sell(e):
    give(e.player, Item(kind=e.kind, count=1))

dialogue apple_stall(p: Player, npc: Person) for npc_named("Apple Seller"):
    when has_room(p, "apple"):
        emit Sell(player=p, kind="apple")
    when not has_room(p, "apple"):
        p: "That's too many apples."

dialogue orange_stall(p: Player, npc: Person) for npc_named("Orange Seller"):
    when has_room(p, "orange"):
        emit Sell(player=p, kind="orange")
    when not has_room(p, "orange"):
        p: "I've already got enough oranges."
"#;

const EX6: &str = r#"
let keeper = npc_named("Mara")

dialogue innkeeper(p: Player, npc: Person) for keeper:
    node greet when once per p:
        npc: f"New face! Welcome to the {here().name}."
        -> menu
    node greet when p.has_tag("outlaw"):
        npc: "Coin first. Then we talk."
        -> menu
    node greet:
        => npc: "Back again?"
        => npc: "Mind the step, it's loose."
        => npc: "Sit anywhere that isn't wet."
        -> menu
    node menu:
        choice:
            "Heard any rumours?":
                detour rumours
            "Tell me a story.":
                detour tale
            once "Who really runs this town?" if act() >= 2:
                npc: "Not the mayor. Ask about the ledger at the mill."
            "Goodbye.":
                npc: "Safe roads."
                return
        -> menu

dialogue rumours(p: Player, npc: Person):
    => npc: "A farmer up the hill lost his dog. Offering a reward, too." when quest_open("lost_dog")
    => npc: "They sealed the old mine again. Third time this year." when quest_open("sealed_mine")
    => npc: "Quiet week. Too quiet."

dialogue tale(p: Player, npc: Person):
    npc: "Once, a traveller sat in an inn much like this one, and the keeper told a tale..."
    when dialogue_depth() < 4:
        choice:
            "And what was that tale?":
                detour tale                  # the dialogue detours into itself
            "Skip to the end."               # no steps: continue after the choice
    npc: "...and when the tale was done, the traveller went on their way."
"#;

const EX7: &str = r#"
# Package `errantry/lost-dog`, quest program
quest LostDog:
    role farmer: Person
    role dog: Creature where dog.has_tag("pet")
    role inn: Person where inn.has_tag("innkeeper")

    let pointed_to_mill: list[Player] = []

    event AskedAround:
        player: Player

    stage Open:
        dialogue dog_rumour(p: Player, npc: Person) for inn topic f"Seen a dog called {dog.name}?" priority 1:
            npc: "Muddy little thing? Ran past at dawn, toward the mill."
            emit AskedAround(player=p)
        @on(AskedAround)
        fn asked(e):
            pointed_to_mill.append(e.player)     # the mill scene can check this later
"#;

#[test]
fn every_spec_snippet_parses() {
    for text in [
        S5_1, S5_3, S5_4, S5_5, S5_6, S5_7_ITEMS, S5_7_STMTS, S5_10, S7, S8_2,
    ] {
        parse_ok(text);
    }
}

#[test]
fn let_forms() {
    let dump = parse_ok(
        "fn main():\n    let x = 1\n    let n: int = 1\n    x = 2\n    *r = v\n    a.b[0] = 1\n    let _ = f()\n",
    );
    assert_eq!(
        dump,
        "(fn main () (let x 1) (let n: int 1) (= x 2) (= (deref r) v) (= (index (field a b) 0) 1) (let _ (call f)))"
    );
}

#[test]
fn else_block_form() {
    let dump = parse_ok(S5_7_STMTS);
    assert_has(
        &dump,
        r#"(let gold (else (call take_gold c 20) (fallback err (block (call log (fstr "failed: " err)) (return)))))"#,
    );
    // In a chain the block belongs to the last link.
    let dump = parse_ok("fn main():\n    let x = a else b else err:\n        return\n");
    assert_eq!(
        dump,
        "(fn main () (let x (else a (else b (fallback err (block (return)))))))"
    );
}

#[test]
fn fn_signatures() {
    assert_eq!(
        parse_ok("fn f(x: int) -> str raises QuestError:\n    pass\n"),
        "(fn f (x: int) -> str raises QuestError (pass))"
    );
    assert_eq!(
        parse_ok("impl L:\n    fn add(&self, n):\n        pass\n"),
        "(impl L (fn add (&self, n) (pass)))"
    );
    assert_eq!(parse_ok("pub fn g():\n    pass\n"), "pub (fn g () (pass))");
    assert_eq!(
        parse_ok("@on(PlayerClicked)\nfn h(e):\n    pass\n"),
        "(@on PlayerClicked) (fn h (e) (pass))"
    );
    assert_eq!(
        parse_ok("fn f(r: &list[int]):\n    return ()\n"),
        "(fn f (r: (& (list int))) (return ()))"
    );
}

#[test]
fn items() {
    let dump = parse_ok(
        "struct S:\n    a: int\n    b: list[str]\n\
         enum E:\n    A\n    B(x: int, y: str)\n\
         error ChestError = ChestEmpty | NotEnough(have: int)\n\
         mod npc\n\
         mod dialogue\n\
         use a::{b, c}\n\
         use a::b as c\n\
         use dialogue::x\n\
         use npc::dialogue::{greet}\n\
         pub let total = 0\n",
    );
    assert_eq!(
        dump,
        "(struct S (a int) (b (list str)))\n\
         (enum E A (B (x int) (y str)))\n\
         (error ChestError ChestEmpty (NotEnough (have int)))\n\
         (mod npc)\n\
         (mod dialogue)\n\
         (use a::b a::c)\n\
         (use (as a::b c))\n\
         (use dialogue::x)\n\
         (use npc::dialogue::greet)\n\
         pub (let total 0)"
    );
}

#[test]
fn match_with_guards_and_alternatives() {
    let dump = parse_ok(
        "fn main():\n    match e:\n        case Stage::Accepted(tick) if tick > 100:\n            pass\n        case A | B:\n            pass\n",
    );
    assert_eq!(
        dump,
        "(fn main () (match e (case (Stage::Accepted tick) (if (> tick 100)) (block (pass))) (case (| A B) (block (pass)))))"
    );
}

#[test]
fn loops_and_jumps() {
    let dump = parse_ok(
        "fn main():\n    for k, v in d.items():\n        continue\n    for i in range(5):\n        break\n    while x:\n        pass\n    return\n    return 1\n    raise NotEnough(have=3)\n",
    );
    assert_eq!(
        dump,
        "(fn main () (for (k v) (call (field d items)) (block (continue))) (for (i) (call range 5) (block (break))) (while x (block (pass))) (return) (return 1) (raise (call NotEnough have=3)))"
    );
}

#[test]
fn top_level_allows_only_items_and_let() {
    let (_, diags) = parse_file("main.qs", "print(\"x\")\n");
    assert_eq!(diags.len(), 1, "{diags:#?}");
    assert_eq!(
        (diags[0].code, diags[0].line, diags[0].col),
        ("syntax", 1, 1)
    );
    for word in ["top level", "`fn`", "`struct`", "`let`"] {
        assert!(diags[0].message.contains(word), "{}", diags[0].message);
    }
}

#[test]
fn error_recovery_reports_both() {
    let text = "fn main():\n    let = 1\n    x =\n    print(\"ok\")\n";
    assert_eq!(codes(text), vec![("syntax", 2), ("syntax", 3)]);
    let (file, _) = parse_file("main.qs", text);
    assert_has(&file.dump(), r#"(call print "ok")"#);
    // One broken header of an `if` chain is one error: its `elif` / `else` clauses go with it.
    let text = "fn main():\n    if x\n        a()\n    elif y:\n        b()\n    else:\n        c()\n    d()\n";
    assert_eq!(codes(text), vec![("syntax", 2)]);
    // An f-string's broken part is recorded, and the parse goes on to the next error.
    let text = "fn main():\n    print(f\"a {x +}\")\n    pass\n    pass\n    let = 1\n";
    assert_eq!(codes(text), vec![("syntax", 2), ("syntax", 5)]);
}

#[test]
fn messages_name_the_expected_token() {
    let (_, diags) = parse_file("main.qs", "fn main():\n    if x\n        pass\n");
    assert_eq!(diags.len(), 1, "{diags:#?}");
    assert_eq!(
        (diags[0].code, diags[0].line, diags[0].col),
        ("syntax", 2, 9)
    );
    assert!(diags[0].message.contains("`:`"), "{}", diags[0].message);
    let (_, diags) = parse_file("main.qs", "fn main():\n    if x:\n    pass\n");
    assert_eq!(diags.len(), 1, "{diags:#?}");
    assert_eq!((diags[0].code, diags[0].line), ("syntax", 3));
    assert!(
        diags[0].message.contains("an indented block"),
        "{}",
        diags[0].message
    );
}

#[test]
fn block_nesting_limit() {
    let mut text = String::from("fn main():\n");
    for i in 1..=300 {
        text.push_str(&"    ".repeat(i));
        text.push_str("if true:\n");
    }
    text.push_str(&"    ".repeat(301));
    text.push_str("pass\n");
    let (_, diags) = parse_file("main.qs", &text);
    let found: Vec<_> = diags.iter().map(|d| d.code).collect();
    assert_eq!(found, vec!["too-deep"]);
}

#[test]
fn spec_dialogue_and_quest_parse() {
    let dump = parse_ok(S5_11);
    for part in [
        "(event Accepted (player Player))",
        "(@pick best) (dialogue farmer_talk (p: Player, npc: Person) (for farmer)",
        r#"(line npc "Have you seen my dog? He ran off toward the hills." farmer.start)"#,
        r#"(option "I'll find him." farmer.accept (if (not accepted)) (emit Accepted player=p) (line npc "Bless you. Bring him back before dark." farmer.thanks))"#,
        r#"(option "What does he look like?" farmer.ask_dog (line npc (fstr "Brown, one ear up. He answers to " dog_name ".") farmer.dog_desc) (jump start))"#,
        r#"(option once "Is that blood on your sleeve?" farmer.blood (detour farmer_secret))"#,
        r#"(option "Goodbye." farmer.goodbye))"#,
        r#"(line npc "Safe roads." farmer.bye)"#,
        r#"(node hello (when (call (field p has_tag) "outlaw")) (line npc "Keep walking." greet.outlaw))"#,
        r#"(node hello (when (once per p)) (line npc "A new face! Welcome." greet.first))"#,
        r#"(node hello (group (=> (line npc "Morning." greet.a)) (=> (line npc "Weather's turning." greet.b)) (=> (line npc "You again?" greet.c (when (once per p))))))"#,
        "(@on Accepted) (fn accept (e) (= accepted true)",
    ] {
        assert_has(&dump, part);
    }
    // The farmer's choice has exactly four options.
    let choice = &dump[dump.find("(choice").unwrap()..];
    let choice = &choice[..choice.find(r#"(line npc "Safe roads.""#).unwrap()];
    assert_eq!(choice.matches("(option").count(), 4, "{choice}");

    let dump = parse_ok(S5_12);
    for part in [
        "(quest LostDog (requires (>= (call act) 1)) (role farmer Person (where (and (== (field farmer occupation) Occupation::Farmer) (field farmer alive))))",
        "(role miller optional Person (where (== (field miller occupation) Occupation::Miller)))",
        "(let attempts 0)",
        "(event Nightfall (attempt int))",
        r#"(stage Open (dialogue plea (p: Player, npc: Person) (for farmer) (topic (fstr "About " (field dog name)) lostdog.topic) (line npc (fstr (field dog name) " ran off toward the hills.") lostdog.plea)"#,
        r#"(option "Not now." lostdog.not_now)"#,
        "(@on Accepted) (fn accepted (e) (call join (field e player)) (jump Search))",
        r#"(stage Search (objective find_dog (fstr "Find " (field dog name)) lostdog.find (at dog))"#,
        r#"(objective ask_miller optional "Ask at the mill" lostdog.mill (at miller))"#,
        "(transition Returning (and (field find_dog done) (field feed_dog done)))",
        "(@on Nightfall) (fn bolted (e) (jump Search))",
        r#"(stage Returning (objective bring_home (fstr "Bring " (field dog name) " home") lostdog.return (at farmer))"#,
        "(part dog_watch on dog (@on PlayerNear) (fn spotted (e)",
        "(call $emit (call DogFound player=(field e player)))",
    ] {
        assert_has(&dump, part);
    }
}

#[test]
fn examples_doc_parses() {
    for text in [EX1, EX2, EX3, EX4, EX5, EX6, EX7] {
        parse_ok(text);
    }
    let dump = parse_ok(EX1);
    assert_has(
        &dump,
        r#"(=> (line npc "Weird how tomorrow is Monday, right?" (when (and (== day Day::Wednesday) (== time TimeOfDay::Evening)))) (line p "did you want the devs to have to write out even more sample content?")"#,
    );
    let dump = parse_ok(EX3);
    assert_has(
        &dump,
        r#"(choice timeout=300 (option "Option 1?" (line npc "option 1 was selected"))"#,
    );
    assert_has(
        &dump,
        r#"(option default (line npc "The hidden fallback option was selected"))"#,
    );
    assert_has(
        &dump,
        r#"(option default "Option 1?" (line npc "option 1 was selected")"#,
    );
    let dump = parse_ok(EX5);
    assert_has(
        &dump,
        r#"(when (call has_room p "apple") (emit Sell player=p kind="apple"))"#,
    );
    let dump = parse_ok(EX6);
    assert_has(&dump, "(dialogue tale (p: Player, npc: Person) (line npc");
    assert_has(&dump, "(detour tale)");
    let dump = parse_ok(EX7);
    assert_has(
        &dump,
        r#"(for inn) (topic (fstr "Seen a dog called " (field dog name) "?") (priority 1))"#,
    );
}

#[test]
fn steps_are_not_statements() {
    let text = "dialogue d(p: Player, npc: Person):\n    npc: \"hi\"\n    let x = 1\n    if x:\n        npc: \"no\"\n    give(p, i)\n    npc: \"bye\"\n";
    let (file, diags) = parse_file("main.qs", text);
    let found: Vec<_> = diags.iter().map(|d| (d.code, d.line)).collect();
    assert_eq!(
        found,
        vec![("not-a-step", 3), ("not-a-step", 4), ("not-a-step", 6)]
    );
    for d in &diags {
        for form in ["choice", "=>", "detour", "emit"] {
            assert!(d.message.contains(form), "{}", d.message);
        }
    }
    assert_has(&file.dump(), r#"(line npc "bye")"#);
    assert_eq!(parse_ok("fn main():\n    \"hi\"\n"), r#"(fn main () "hi")"#);
}

#[test]
fn dialogue_grammar_has_no_conflicts() {
    // The grammar is generated at build time and LALRPOP refuses any conflict, so building is the check.
    let dump = parse_ok(
        "dialogue d(p: Player, npc: Person):\n    p: \"Hey\"\n    \"Narration.\" @d.n\n    choice(timeout=10):\n        \"Plain.\"\n        once \"Once.\" @d.once if x > 1:\n            npc: \"ok\"\n        default \"Default.\":\n            -> start\n        default:\n            return\n    => npc: \"a\" when once per p if y:\n        detour other\n    once per p:\n        emit E(a=1)\n    when x:\n        \"b\"\n",
    );
    assert_eq!(
        dump,
        "(dialogue d (p: Player, npc: Person) (line p \"Hey\") (line \"Narration.\" d.n) \
         (choice timeout=10 (option \"Plain.\") (option once \"Once.\" d.once (if (> x 1)) (line npc \"ok\")) \
         (option default \"Default.\" (jump start)) (option default (return))) \
         (group (=> (line npc \"a\" (when (once per p if y))) (detour other))) \
         (once per p (emit E a=1)) (when x (line \"b\")))"
    );
}
// The python blocks of `2026-11-16-quest-examples.md`, copied verbatim (fixtures QX1…QX6).

const QX1: &str = r##"
# Package errantry/lost-dog — src/main.qs

let NIGHT_TICKS = 9000                  # from finding the dog to it bolting, unless fed

quest LostDog:
    requires act() >= 1
    role farmer: Person where farmer.occupation == Occupation::Farmer and farmer.alive
    role dog: Creature where dog.has_tag("pet") and dog.owner == farmer
    optional role miller: Person where miller.occupation == Occupation::Miller and near(miller, farmer, regions=1)

    let attempts = 0                    # quest state: how often the dog was found and lost again

    event Accepted:
        player: Player
    event AskedMiller:
        player: Player
    event DogFound:
        player: Player
    event DogFed:
        player: Player
    event Nightfall:
        attempt: int
    event Reunited:
        player: Player

    # ── Open: the farmer asks anyone who talks to him ─────────────────────────
    stage Open:
        dialogue worry(p: Player, npc: Person) for farmer:          # a greeting: no topic
            node hi when once per p:
                npc: "Stranger! Have you seen a brown dog on the road?"         @lostdog.worry.first
            node hi:
                => npc: "Still no sign of him."                                 @lostdog.worry.a
                => npc: "I left the gate open. My own fault."                   @lostdog.worry.b

        dialogue plea(p: Player, npc: Person) for farmer topic f"About {dog.name}" @lostdog.topic:
            npc: f"{dog.name} ran off toward the hills two days ago."           @lostdog.plea
            npc: "He's never been gone a night before."                         @lostdog.plea2
            choice:
                "I'll find him."                                                @lostdog.accept:
                    emit Accepted(player=p)
                    npc: "Bless you. Take something to eat. He's skittish."     @lostdog.hint
                "What does he look like?"                                       @lostdog.ask_look:
                    npc: "Brown, one ear up. Answers to his name, mostly."      @lostdog.look
                "Not now."                                                      @lostdog.not_now

        @on(Accepted)
        fn accepted(e):
            join(e.player)
            -> Search

    # ── Search: two objectives in parallel, one optional ─────────────────────
    stage Search:
        objective find_dog f"Find {dog.name}"                    @lostdog.find     at dog
        objective feed_dog f"Coax {dog.name} with food"          @lostdog.feed     at dog
        optional objective ask_miller "Ask at the mill"          @lostdog.ask_mill at miller
        -> Returning when find_dog.done and feed_dog.done

        dialogue join_in(p: Player, npc: Person) for farmer topic "Can I help look?" @lostdog.join if not (p in participants):
            npc: "Every pair of eyes helps."                                    @lostdog.join_ok
            emit Accepted(player=p)

        dialogue lost_again(p: Player, npc: Person) for farmer:
            node hi when attempts >= 2:
                npc: f"Found and lost {attempts} times now. He's frightened."   @lostdog.again
            node hi:
                npc: "Any luck?"                                                @lostdog.any_luck

        dialogue tip(p: Player, npc: Person) for miller topic f"Seen {dog.name}?" @lostdog.mill_topic if p in participants:
            npc: "Something's been at my grain sacks down by the weir."         @lostdog.mill_tip
            emit AskedMiller(player=p)

        @on(Entered)
        fn count_attempt(e):
            attempts = attempts + 1

        @on(Accepted)
        fn joined_late(e):
            join(e.player)

        @on(AskedMiller)
        fn asked(e):
            ask_miller.finish()

        @on(DogFound)
        fn found(e):
            if not find_dog.done:       # two players may spot him in one tick
                find_dog.finish()
                after(NIGHT_TICKS, Nightfall(attempt=attempts))

        @on(DogFed)
        fn fed(e):
            if find_dog.done:
                feed_dog.finish()
                follow(dog, e.player)

        @on(Nightfall)
        fn bolted(e):
            -> Search                   # a failed attempt: re-entering re-opens both objectives

    # ── Returning: bring him home ────────────────────────────────────────────
    stage Returning:
        objective bring_home f"Bring {dog.name} home to {farmer.name}"   @lostdog.return at farmer

        dialogue reunion(p: Player, npc: Person) for farmer topic f"{dog.name} is back" @lostdog.back_topic if near(dog, farmer, blocks=8):
            npc: f"{dog.name}! You daft mutt."                                  @lostdog.reunion
            npc: "Take this. My father carved it. It's all I have of worth."    @lostdog.reward
            emit Reunited(player=p)

        @on(Reunited)
        fn reunited(e):
            bring_home.finish()
            for q in participants:
                give(q, Item(kind="carved_whistle", count=1))
            complete()                  # for every participant

    # ── In any stage ─────────────────────────────────────────────────────────
    @on(RoleLost)
    fn lost(e):
        match e.role:
            case Role::miller:
                pass                    # optional: the tip is simply gone
            case _:
                fail()                  # the farmer or the dog: the quest cannot be done

    # ── The part that rides with the dog ─────────────────────────────────────
    part dog_watch on dog:
        @on(PlayerNear)
        fn spotted(e):
            if stage == Stage::Search and e.player in participants and not find_dog.done:
                emit DogFound(player=e.player)

        @on(ItemGiven)
        fn offered(e):
            if stage == Stage::Search and e.item.kind == "meat":
                emit DogFed(player=e.player)
"##;

const QX2: &str = r##"
# Package errantry/act-two — src/sealed_mine.qs

quest SealedMine at elder:
    requires act() == 2
    role elder: Person where elder.occupation == Occupation::Elder and elder.alive
    role foreman: Person where foreman.occupation == Occupation::Miner and foreman.site == elder.site
    role mine: Site where site_kind(mine) == SiteKind::Mine and near(mine, elder, regions=2)

    event HeardOut:
        player: Player
    event LurkerSlain:
        player: Player
    event SealSet:
        player: Player

    scene argument(a: Person, b: Person) uninterruptible:
        a: "The seal held for forty years. You will not break it for silver."
        b: "Silver feeds this village, old man."
        pause 30
        a: "Silver did not feed the eleven we lost."
        b: "..."                                                      (ticks=60)
        a: "Then let the stranger decide what they find down there."

    stage Council:
        objective hear_out "Hear the elder and the foreman out" at elder
        -> Depths when hear_out.done

        dialogue ask(p: Player, npc: Person) for elder topic "The mine?":
            npc: "Brann wants it open. Stand here and listen to him lie."
            emit HeardOut(player=p)

        @on(HeardOut)
        fn convene(e):
            join(e.player)
            play(argument, a=elder, b=foreman)

        @on(SceneEnded)
        fn heard(e):
            if e.scene == argument and e.reason == SceneEnd::Finished:
                hear_out.finish()

    stage Depths:
        objective lurkers "Clear the lurkers from the mine" at mine count 6
        objective seal "Set the seal stone" at mine

        dialogue foreman_sulk(p: Player, npc: Person) for foreman:
            => npc: "Go on then. Bury our living."
            => npc: "Mind the third shaft. It floods."

        @on(LurkerSlain)
        fn slain(e):
            join(e.player)              # anyone who fights down there takes part
            lurkers.progress(1)

        @on(SealSet)
        fn sealed(e):
            if lurkers.done:
                seal.finish()
                complete()

    part depths at mine:
        @on(Setup)
        fn nest(e):
            place(prefab("lurker_nest"), mine) else:
                decline()               # no room at this mine: the whole quest is not placed
                return

        @on(Killed)
        fn killed(e):
            if stage == Stage::Depths and ours(e.victim):
                match e.by:
                    case Some(p):
                        emit LurkerSlain(player=p)
                    case None:
                        pass            # drowned in the third shaft: no one's kill

        @on(BlockUsed)
        fn used(e):
            if stage == Stage::Depths and e.tag == "seal_stone":
                emit SealSet(player=e.player)
"##;

const QX3: &str = r##"
# Package errantry/bandits — src/main.qs

repeatable quest BanditCamp(cooldown=72000, exclusive=true) at camp:
    role watch: Person where watch.occupation == Occupation::Guard and watch.alive
    role camp: Place where camp.open_ground(blocks=20) and near(camp, watch, regions=1)

    let veteran = false                 # re-initialised on every reset; set from the namespace in Setup
    let paid: list[Player] = []

    event Hired:
        player: Player
    event Paid:
        player: Player

    @on(Setup)
    fn pitch_camp(e):
        let beaten = await world.get("times_beaten") else 0     # the namespace survives resets
        veteran = beaten >= 3
        let camp_prefab = prefab("bandit_camp")
        if veteran:
            camp_prefab = prefab("bandit_camp_veteran")
        place(camp_prefab, camp) else:
            decline()
            return

    stage Notice:
        dialogue hire(p: Player, npc: Person) for watch topic "Any work going?":
            when veteran:
                npc: "Them again. Better blades this time, I hear."
            npc: "Bandits in the hollow. Clear them out and the village pays thirty silver."
            choice:
                "Consider it done.":
                    emit Hired(player=p)
                "Not today."

        @on(Hired)
        fn hired(e):
            join(e.player)
            -> Assault

    stage Assault:
        objective bandits "Drive the bandits from the hollow" at camp count 5
        optional objective banner "Take their banner" at camp
        -> Payday when bandits.done

        @on(Killed)
        fn killed(e):
            if ours(e.victim):
                bandits.progress(1)

        @on(PlayerDefeated)
        fn down(e):
            leave(e.player)
            if participants.len() == 0:
                -> Notice               # everyone is down: the attempt failed; the job is open again

        @on(BlockUsed)
        fn banner_taken(e):
            if e.tag == "bandit_banner":
                banner.finish()

    stage Payday:
        dialogue pay(p: Player, npc: Person) for watch topic "The hollow is clear." if p in participants and not (p in paid):
            npc: "Good work. Thirty, as promised."
            when banner.done:
                npc: "And the banner? The reeve will want that. Five more."
            emit Paid(player=p)

        @on(Paid)
        fn pay_out(e):
            let silver = 30
            if banner.done:
                silver = 35
            give(e.player, Item(kind="silver", count=silver))
            paid.append(e.player)
            if paid.len() == participants.len():
                await world.add("times_beaten", 1)
                complete()
"##;

const QX4_LIBRARY: &str = r##"
# Package errantry/trade — src/haggle.qs   (a library: no quest)

pub dialogue haggle(p: Player, npc: Person) exits agreed, refused:
    npc: "Times are hard. You know how it is."
    choice:
        "Half now, half at harvest?" if p.skill("trade") >= 2:
            npc: "...Half now. Fine."
            return agreed
        "Pay, or I take it from your stock." if p.skill("intimidate") >= 3:
            npc: "All right! All right."
            return agreed
        "Then I'll be back.":
            return refused
    # falling off the end would be the implicit exit `done`; every option returns here
"##;

const QX4: &str = r##"
# Package errantry/old-debts — src/main.qs   (depends on errantry/trade)
use trade::haggle::haggle          # `trade`: the dependency's crate name, as in Rust

quest OldDebt:
    role lender: Person where lender.occupation == Occupation::Merchant
    role debtor: Person where debtor.site == lender.site and debtor != lender and debtor.alive

    event Collected:
        player: Player

    stage Collect:
        objective collect f"Collect {lender.name}'s money from {debtor.name}" at debtor

        dialogue confront(p: Player, npc: Person) for debtor topic f"{lender.name} sent me" exits paid, stalled:
            npc: "Oh. Them."
            detour haggle:
                agreed:
                    npc: "Here. Tell them we're square."
                    return paid
                refused:
                    npc: "Come back when you're less... you."
                    return stalled

        dialogue thanks(p: Player, npc: Person) for lender topic "About the debt" if collect.done:
            npc: "You got it? Then here's your cut."
            emit Collected(player=p)

        @on(DialogueEnded)
        fn after_talk(e):
            if e.dialogue == confront and e.exit == confront::paid:
                join(e.player)
                collect.finish()

        @on(Collected)
        fn reward(e):
            give(e.player, Item(kind="silver", count=10))
            complete()
"##;

const QX5: &str = r##"
# Package errantry/town-life — src/main.qs   (no quest: package-level content)

dialogue smith_hello(p: Player, npc: Person) for npc where npc.occupation == Occupation::Smith:
    node hello when once per p:
        npc: "Mind the sparks. New faces always stand too close."
    node hello when p.inventory_count("broken_tool") > 0:
        npc: "That's a sorry tool you're carrying. Leave it with me."
    node hello:
        => npc: "Hot work, this."
        => npc: "Need something mended?"
        => npc: "Iron's dear since the mine closed." when act() >= 3

scene market_haggle(a: Person, b: Person) where a.occupation == Occupation::Merchant and b.site == a.site and a != b:
    b: "How much for the turnips?"
    a: "Two coppers."
    b: "Two? They're half rot."
    pause 20
    a: "Then it's one copper of turnip and one of rot."                     (ticks=90)
    -> market_haggle_after

scene market_haggle_after(a: Person, b: Person):
    pause 40
    b: "...Fine. Two."

scene crier(npc: Person) where npc.has_tag("town_crier"):                 # a one-actor scene: a bark
    => npc: "Hear ye, hear ye!"
    => npc: "The hollow road is closed by order of the reeve!" when act() == 1
    => npc: "The old mine is sealed again! Praise the stranger!" when completed("errantry/act-two::SealedMine")
"##;

const QX6_BAD_WHERE: &str = r##"
fn wealthy(x: Person) -> bool:
    return x.skill("trade") > 5

quest Q:
    role buyer: Person where wealthy(buyer)            # bad-where: a script fn is not a fact query
"##;

const QX6_STATELESS: &str = r##"
let rumour = "wolves"                                  # a global in a package with no quest

dialogue gossip(p: Player, npc: Person) for npc where npc.has_tag("gossip"):
    npc: f"They say {rumour}."                         # stateless-dialogue: reads a script global
    emit Gossiped(player=p)                            # stateless-dialogue: no instance to emit to
"##;

const QX6_UNKNOWN_EXIT: &str = r##"
dialogue offer(p: Player, npc: Person) for trader:
    detour haggle:
        agreed:
            npc: "Done."
        cheated:                                       # unknown-exit: haggle exits agreed, refused
            npc: "Hey!"
"##;

const QX6_SCENE_CHOICE: &str = r##"
scene quarrel(a: Person, b: Person):
    a: "Well?"
    choice:                                            # scene-choice: a scene has nobody to choose
        "Yes"
"##;

const QX6_DUPLICATE: &str = r##"
quest Q:
    role dog: Creature
    stage Search:
        objective dog "Find the dog" at dog            # duplicate-def: the objective `dog` shadows the role
    @on(Entered)                                       # entered-outside-stage: whose stage was entered?
    fn entered(e):
        pass
"##;

#[test]
fn quest_header_forms() {
    let dump = parse_ok(
        "repeatable quest B(cooldown=72000, exclusive=true) at camp:\n    let x = 1\n\
         quest C at elder:\n    let y = 1\n\
         pub quest D:\n    let z = 1\n\
         pub repeatable quest E:\n    let w = 1\n",
    );
    assert_eq!(
        dump,
        "(quest B repeatable (cooldown 72000) (exclusive true) at camp (let x 1))\n\
         (quest C at elder (let y 1))\n\
         pub (quest D (let z 1))\n\
         pub (quest E repeatable (let w 1))"
    );
}

#[test]
fn objectives_named_counted_optional() {
    let dump = parse_ok(
        "quest Q:\n    stage S:\n        objective lurkers \"Clear the lurkers\" at mine count 6\n\
         \x20       optional objective banner \"Take their banner\" @b.banner at camp\n\
         \x20       objective seal f\"Seal {mine.name}\" @q.at\n",
    );
    assert_eq!(
        dump,
        "(quest Q (stage S \
         (objective lurkers \"Clear the lurkers\" (at mine) (count 6)) \
         (objective banner optional \"Take their banner\" b.banner (at camp)) \
         (objective seal (fstr \"Seal \" (field mine name)) q.at)))"
    );
}

#[test]
fn transition_is_a_stage_member() {
    let dump = parse_ok(
        "quest Q:\n    stage S:\n        -> T when a.done and b.done\n\
         \x20       @on(X)\n        fn go(e):\n            -> T\n\
         \x20   stage T:\n        -> S when false\n",
    );
    assert_eq!(
        dump,
        "(quest Q (stage S (transition T (and (field a done) (field b done))) \
         (@on X) (fn go (e) (jump T))) (stage T (transition S false)))"
    );
    // A transition needs its condition; a bare `-> T` belongs in a handler.
    assert_eq!(
        codes("quest Q:\n    stage S:\n        -> T\n"),
        vec![("syntax", 3)]
    );
}

#[test]
fn part_on_and_at() {
    let dump = parse_ok(
        "quest Q:\n    part watch on dog:\n        let seen = 0\n        event Spotted:\n            p: Player\n\
         \x20       @on(PlayerNear)\n        fn near(e):\n            emit Spotted(p=e.player)\n\
         \x20   part depths at mine:\n        @on(Setup)\n        fn nest(e):\n            pass\n",
    );
    assert_eq!(
        dump,
        "(quest Q (part watch on dog (let seen 0) (event Spotted (p Player)) \
         (@on PlayerNear) (fn near (e) (call $emit (call Spotted p=(field e player))))) \
         (part depths at mine (@on Setup) (fn nest (e) (pass))))"
    );
}

#[test]
fn scene_with_where_pause_ticks() {
    let dump = parse_ok(
        "scene haggle(a: Person, b: Person) where a != b:\n    b: \"How much?\"\n    pause 20\n\
         \x20   a: \"Two.\" @h.two (ticks=60) when x\n    -> after\n\
         pub scene argue(a: Person) uninterruptible:\n    => a: \"...\" (ticks=90)\n\
         scene both(a: Person) where a.idle uninterruptible:\n    a: \"x\"\n",
    );
    assert_eq!(
        dump,
        "(scene haggle (a: Person, b: Person) (where (!= a b)) (line b \"How much?\") (pause 20) \
         (line a \"Two.\" h.two (ticks 60) (when x)) (jump after))\n\
         pub (scene argue (a: Person) uninterruptible (group (=> (line a \"...\" (ticks 90)))))\n\
         (scene both (a: Person) (where (field a idle)) uninterruptible (line a \"x\"))"
    );
    // Inside a quest, and with `@pick`.
    let dump = parse_ok("quest Q:\n    @pick(first)\n    scene s(a: Person):\n        a: \"x\"\n");
    assert_eq!(
        dump,
        "(quest Q (@pick first) (scene s (a: Person) (line a \"x\")))"
    );
    // `(ticks=n)` takes only `ticks`.
    assert_eq!(
        codes("scene s(a: Person):\n    a: \"x\" (tick=3)\n"),
        vec![("syntax", 2)]
    );
}

#[test]
fn dialogue_exits_and_detour_arms() {
    let dump = parse_ok(
        "dialogue d(p: Player, npc: Person) for debtor topic \"Debt\" exits paid, stalled:\n\
         \x20   detour haggle:\n        agreed:\n            npc: \"Here.\"\n            return paid\n\
         \x20       refused:\n            return stalled\n    detour other\n",
    );
    assert_eq!(
        dump,
        "(dialogue d (p: Player, npc: Person) (for debtor) (topic \"Debt\") (exits paid stalled) \
         (detour haggle (arm agreed (line npc \"Here.\") (return paid)) (arm refused (return stalled))) \
         (detour other))"
    );
    let dump =
        parse_ok("pub dialogue h(p: Player, npc: Person) exits agreed, refused,:\n    return\n");
    assert_eq!(
        dump,
        "pub (dialogue h (p: Player, npc: Person) (exits agreed refused) (return))"
    );
}

#[test]
fn return_exit() {
    let dump = parse_ok(
        "dialogue d(p: Player, npc: Person) exits a:\n    choice:\n        \"x\":\n            return a\n        \"y\":\n            return\n",
    );
    assert_eq!(
        dump,
        "(dialogue d (p: Player, npc: Person) (exits a) (choice (option \"x\" (return a)) (option \"y\" (return))))"
    );
}

#[test]
fn for_npc_where() {
    let dump = parse_ok(
        "dialogue s(p: Player, npc: Person) for npc where npc.occupation == Occupation::Smith topic \"Work?\":\n    npc: \"x\"\n",
    );
    assert_eq!(
        dump,
        "(dialogue s (p: Player, npc: Person) (for npc) (where (== (field npc occupation) Occupation::Smith)) \
         (topic \"Work?\") (line npc \"x\"))"
    );
}

#[test]
fn contextual_words_stay_identifiers() {
    let dump = parse_ok(
        "fn pause(x):\n    return x\n\
         fn main():\n    let scene = 1\n    let part = 2\n    count = 3\n    pause(4)\n    scene = part\n\
         \x20   let requires = optional\n    repeatable = exits + on + uninterruptible\n    requires.x = 1\n",
    );
    assert_eq!(
        dump,
        "(fn pause (x) (return x))\n\
         (fn main () (let scene 1) (let part 2) (= count 3) (call pause 4) (= scene part) \
         (let requires optional) (= repeatable (+ (+ exits on) uninterruptible)) (= (field requires x) 1))"
    );
    assert_eq!(
        parse_ok("let scene = 1\nlet part = 2\nlet count = 3\n"),
        "(let scene 1)\n(let part 2)\n(let count 3)"
    );
}

#[test]
fn quest_examples_doc_parses() {
    for text in [
        QX1,
        QX2,
        QX3,
        QX4_LIBRARY,
        QX4,
        QX5,
        QX6_BAD_WHERE,
        QX6_STATELESS,
        QX6_UNKNOWN_EXIT,
        QX6_SCENE_CHOICE,
        QX6_DUPLICATE,
    ] {
        parse_ok(text);
    }
    let dump = parse_ok(QX1);
    assert_has(&dump, "(role miller optional Person (where (and");
    assert_has(&dump, "(part dog_watch on dog (@on PlayerNear)");
    let dump = parse_ok(QX2);
    assert_has(
        &dump,
        "(quest SealedMine at elder (requires (== (call act) 2))",
    );
    assert_has(
        &dump,
        r#"(scene argument (a: Person, b: Person) uninterruptible (line a "The seal held for forty years. You will not break it for silver.")"#,
    );
    assert_has(
        &dump,
        r#"(pause 30) (line a "Silver did not feed the eleven we lost.") (line b "..." (ticks 60))"#,
    );
    assert_has(
        &dump,
        r#"(objective lurkers "Clear the lurkers from the mine" (at mine) (count 6))"#,
    );
    assert_has(&dump, "(transition Depths (field hear_out done))");
    assert_has(&dump, "(fallback _ (block (call decline) (return)))");
    let dump = parse_ok(QX3);
    assert_has(
        &dump,
        "(quest BanditCamp repeatable (cooldown 72000) (exclusive true) at camp",
    );
    let dump = parse_ok(QX4_LIBRARY);
    assert_has(
        &dump,
        "pub (dialogue haggle (p: Player, npc: Person) (exits agreed refused)",
    );
    let dump = parse_ok(QX4);
    assert_has(
        &dump,
        "(exits paid stalled) (line npc \"Oh. Them.\") (detour haggle (arm agreed",
    );
    let dump = parse_ok(QX5);
    assert_has(
        &dump,
        "(for npc) (where (== (field npc occupation) Occupation::Smith))",
    );
    assert_has(&dump, "(jump market_haggle_after))");
    let dump = parse_ok(QX6_UNKNOWN_EXIT);
    assert_has(&dump, "(arm cheated (line npc \"Hey!\"))");
    let dump = parse_ok(QX6_DUPLICATE);
    assert_has(&dump, "(objective dog \"Find the dog\" (at dog))");
}

/// Review B (whole-branch, Task 52, minor): `requires (a or b) and c` was a syntax error — a `(` after
/// `requires` did not begin its expression. A spaced `(` does; `requires(x)` stays a call.
#[test]
fn requires_may_begin_with_a_parenthesis() {
    let dump = parse_ok(
        "quest Q:\n    requires (act() >= 1 or act() >= 2) and act() < 9\n    stage A:\n        @on(Entered)\n        fn a(e):\n            pass\n",
    );
    assert_has(&dump, "(requires (and (or");
    let dump = parse_ok("fn main():\n    requires(1)\n");
    assert_has(&dump, "(call requires 1)");
}
