fn main():
    let a = [1]
    let r = &a
    if true:
        let b = [2, 3]
        r = &b
    r.append(4)
    print(str(a.len()))
