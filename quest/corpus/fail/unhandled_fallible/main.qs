fn check(n: int) -> int:
    if n > 3:
        raise TooBig
    return n

fn main():
    let r = check(5)
    check(2)
