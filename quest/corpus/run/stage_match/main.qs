# Spec §5.6: enums, exhaustive match with variants, guards, `_`, literals, `|`, struct patterns, Option.
enum Stage:
    Asked
    Accepted(tick: int)
    Done

struct Pos:
    x: int
    y: int

fn talk(p: Player, stage: Stage):
    match stage:
        case Stage::Asked:
            say(p, "Will you help?")
        case Stage::Accepted(tick) if tick > 100:
            say(p, "Still waiting on you.")
        case Stage::Accepted(_):
            pass
        case Stage::Done:
            say(p, "Thanks again.")

fn size(n: int) -> str:
    match n:
        case 0:
            return "none"
        case 1 | 2:
            return "few"
        case _:
            return "many"

fn where_is(p: Pos) -> str:
    match p:
        case Pos(x=0, y=0):
            return "origin"
        case Pos(x=0, y=y):
            return f"on the y axis at {y}"
        case Pos(x, y=_):
            return f"x={x}"

fn first_word(words: list[str]) -> str:
    let w = words.pop()
    match w:
        case Some(s):
            return s
        case None:
            return "(nothing)"

fn main():
    let p = Player(id=1, name="ana")
    talk(p, Stage::Asked)
    talk(p, Stage::Accepted(50))
    talk(p, Stage::Accepted(tick=150))
    talk(p, Stage::Done)
    print(f"{size(0)} {size(2)} {size(9)}")
    print(where_is(Pos(x=0, y=0)))
    print(where_is(Pos(x=0, y=4)))
    print(where_is(Pos(x=3, y=4)))
    print(first_word(["a", "b"]))
    print(first_word([]))
    let s = Stage::Accepted(tick=3)
    print(f"{s} {Stage::Done}")
