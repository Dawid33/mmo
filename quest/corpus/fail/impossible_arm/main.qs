fn take(n: int) -> int:
    if n < 0:
        raise Negative
    return n

fn main():
    match take(1):
        case Ok(n):
            print(str(n))
        case Err(Negative):
            print("negative")
        case Err(TooBig):
            print("too big")
