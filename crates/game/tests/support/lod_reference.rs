//! The pre-brick LOD downsampler (2026-10-29), kept verbatim as the reference
//! the brick-view path (`game::lod::downsample_chunks`) must match byte for
//! byte. It reads a chunk as ONE 32³ grid of 2×2×2 blocks, so it is only a
//! valid reference while `CHUNK_BLOCKS == BRICK_BLOCKS`; callers skip it
//! otherwise. Shared by `crates/game/tests/lod_streaming.rs` and
//! `crates/worldgen/tests/lod_bricks.rs` through `#[path]` (integration tests
//! cannot see the lib's `#[cfg(test)]` items).
