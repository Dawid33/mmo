# `bad-line-text`: a line's text is a string or f-string literal (it is translated by its id), never a computed
# value (dialogue examples §5).

let hal = Person(id=3)
let refusal = "No."

fn polite() -> str:
    return "Please."

dialogue hello(p: Player, npc: Person) for hal:
    npc: refusal @bad.var
    p: polite() @bad.call
