struct Position {
    x: f32,
    y: f32,
    z: f32,
}

struct Velocity {
    x: f32,
    y: f32,
    z: f32,
}

pub struct Player {
    id: usize,
    position: Position,
    velocity: Velocity,
}
