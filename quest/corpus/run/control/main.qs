# Spec §5.4: functions with inferred types, if/elif/else, while, for over range/list/dict, break, continue,
# return, pass. The spec's example verbatim, then the other loop forms.
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
    for n in [1, 2, 3, 4, 5]:
        if n % 2 == 0:
            continue
        print(str(n))
    for n in range(2, 4):
        pass
    let ages = {"ana": 30, "bo": 12}
    ages["cy"] = 7
    for k, v in ages.items():
        print(f"{k}={v}")
    for k in ages:
        print(k)
