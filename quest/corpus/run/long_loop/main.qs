#! fuel=200 ticks=100000
# Spec §6.2: a long loop under a small fuel budget pauses at instruction boundaries and continues next tick.
fn main():
    let total = 0
    let i = 0
    while i < 50000:
        total = total + i
        i = i + 1
    print(f"total {total}, i {i}")
