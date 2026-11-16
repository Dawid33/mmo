# Spec §5.9: int division truncates toward zero, `%` is Rust's remainder, floats print shortest round-trip
# (Rust `{:?}`), `libm` transcendentals, NaN canonical; scalar `==` is IEEE, inside a list it compares bits
# (plan decision 12).
fn main():
    print(f"{7 / 2} {-7 / 2} {7 % 3} {-7 % 3} {7 % -3}")
    print(f"{7.0 / 2.0} {1.0} {0.1 + 0.2} {7.5 % 2.0}")
    print(f"{sqrt(2.0)} {sin(0.0)} {sin(1.0)} {cos(0.0)} {pow(2.0, 10.0)}")
    let nan = sqrt(-1.0)
    print(f"{nan} {nan == nan} {[nan] == [nan]}")
    let z = -0.0
    print(f"{z} {z == 0.0} {[z] == [0.0]}")
    print(f"{1.0 / 0.0} {-1.0 / 0.0}")
    print(f"{float(3)} {int(3.9)} {int(-3.9)}")
    print(f"{abs(-5)} {abs(-2.5)} {min(2, 9)} {max(2.5, 1.0)}")
    print(f"{floor(2.7)} {ceil(2.1)} {round(2.5)} {round(-2.5)}")
    print(f"{9223372036854775807} {1_000} {0xff} {1e3}")
