# Spec §5.11 "Recursion": detours may form cycles; the runner's explicit return stack makes that safe. `ping`
# and `pong` detour into each other until the player has had enough; the endings unwind in reverse.

let hal = Person(id=3)

dialogue ping(p: Player, npc: Person) for hal:
    npc: "Ping?" @cycle.ping
    choice:
        "Pong." @cycle.to_pong:
            detour pong
        "Enough." @cycle.enough_ping
    npc: "Ping done." @cycle.ping_done

dialogue pong(p: Player, npc: Person):
    npc: "Pong?" @cycle.pong
    choice:
        "Ping." @cycle.to_ping:
            detour ping
        "Enough." @cycle.enough_pong
    npc: "Pong done." @cycle.pong_done
