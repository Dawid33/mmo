//! Tunables of the quest VM and compiler (design D1). Named consts, never literals elsewhere.

/// Instance memory (256 KiB).
pub const MEM_BYTES_DEFAULT: u32 = 262_144;

/// Header size in words.
pub const HEADER_WORDS: u32 = 64;

/// Runtime heap for queued event payloads and promise answers.
pub const INBOX_BYTES: u32 = 8_192;

/// Command buffer of one segment.
pub const CMD_BYTES: u32 = 8_192;

/// Below this `Layout::plan` fails `MemTooSmall`.
pub const HEAP_MIN_BYTES: u32 = 4_096;

/// Queued events.
pub const QUEUE_LEN: usize = 4;

/// Promise records.
pub const PROMISE_MAX: usize = 16;

/// Parked handlers.
pub const PARKED_MAX: usize = 4;

/// Live timers (`after`) of one instance; one more faults `TooManyTimers`.
pub const TIMERS_MAX: usize = 16;

/// Error return trace entries (later ones are counted, not kept).
pub const ERR_TRACE_MAX: usize = 16;

/// A request unanswered this long resolves `Timeout`.
pub const PROMISE_TIMEOUT_TICKS: u64 = 200;

/// CLI default fuel per tick.
pub const FUEL_PER_TICK_DEFAULT: u64 = 2_000;

/// Children released per reap step.
pub const REAP_CHILDREN_MAX: usize = 16;

/// Most steps of a reference path (`&a.b[i]…`); values themselves have no nesting cap (decision 17).
pub const REF_PATH_MAX: u32 = 32;

/// The most words one structural walk (compare, hash, lookup) may visit.
pub const WORK_WORDS_MAX: u32 = 65_536;

/// Fields of a struct or variant.
pub const STRUCT_FIELDS_MAX: usize = 32;

/// Expression / block nesting in source (`quest-lang`).
pub const PARSE_DEPTH_MAX: usize = 256;

/// A read set (dialogue or part) above this many static words gets the `mirror-size` warning (`quest-lang`).
pub const MIRROR_WORDS_WARN: u32 = 256;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tunables_pinned() {
        assert_eq!(MEM_BYTES_DEFAULT, 262_144);
        assert_eq!(HEADER_WORDS, 64);
        assert_eq!(INBOX_BYTES, 8_192);
        assert_eq!(CMD_BYTES, 8_192);
        assert_eq!(HEAP_MIN_BYTES, 4_096);
        assert_eq!(QUEUE_LEN, 4);
        assert_eq!(PROMISE_MAX, 16);
        assert_eq!(PARKED_MAX, 4);
        assert_eq!(TIMERS_MAX, 16);
        assert_eq!(ERR_TRACE_MAX, 16);
        assert_eq!(PROMISE_TIMEOUT_TICKS, 200);
        assert_eq!(FUEL_PER_TICK_DEFAULT, 2_000);
        assert_eq!(REAP_CHILDREN_MAX, 16);
        assert_eq!(REF_PATH_MAX, 32);
        assert_eq!(WORK_WORDS_MAX, 65_536);
        assert_eq!(STRUCT_FIELDS_MAX, 32);
        assert_eq!(PARSE_DEPTH_MAX, 256);
        assert_eq!(MIRROR_WORDS_WARN, 256);
    }
}
