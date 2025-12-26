use crate::math::Real;

pub fn inv(val: Real) -> Real {
    if val == 0.0 {
        0.0.into()
    } else {
        Real::from(1.0) / val
    }
}
