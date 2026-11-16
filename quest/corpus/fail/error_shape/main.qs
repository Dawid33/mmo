fn a(n: int) -> int:
    if n > 1:
        raise TooBig(limit=1)
    return n

fn b(s: str) -> str:
    if s == "":
        raise TooBig
    return s

fn main():
    print(str(a(1) else 0))
    print(b("x") else "")
