# Spec §5.1: `let` makes a binding and shadows, `=` reassigns the nearest one, block scope, top-level `let`.
let count = 0

fn bump():
    count = count + 1

fn main():
    let x = 1
    print(str(x))
    x = 2
    print(str(x))
    let x = "now a string"
    print(x)
    if true:
        let x = 10
        x = x + 1
        print(str(x))
    print(x)
    let y = 1
    if y == 1:
        y = 5
        let z = 3
        print(str(z))
    print(str(y))
    bump()
    bump()
    print(f"count {count}")
