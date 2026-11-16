# Spec §5.3: every value is a copy; `&x` is a reference (also into places), auto-dereferenced, `*r = v`.
struct Reward:
    item: str
    count: int

struct Quest:
    rewards: list[Reward]

fn add_bonus(r: &Reward, n: int):
    r.count = r.count + n

fn main():
    let a = [1, 2]
    let b = a
    b.append(3)
    print(f"{a} {b}")
    let r = &a
    r.append(9)
    print(f"{a} {r.len()}")
    let quest = Quest(rewards=[Reward(item="gold", count=1), Reward(item="wine", count=2), Reward(item="map", count=3)])
    let saved = quest
    let third = &quest.rewards[2]
    third.count = 30
    add_bonus(&quest.rewards[0], 5)
    print(f"{quest.rewards}")
    print(f"{saved.rewards[2]}")
    let n = 1
    let rn = &n
    *rn = 7
    print(f"{n} {*rn}")
