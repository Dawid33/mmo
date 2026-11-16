use super::title

fn line(s: str) -> str:
    return s + "."

pub fn greet(p: Player):
    say(p, self::line(f"Welcome, I am {title()}"))

pub fn farewell(p: Player):
    say(p, crate::npc::talk::line("Safe roads"))
