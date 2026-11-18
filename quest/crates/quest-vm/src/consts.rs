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

/// Fuel of one pure evaluation by the dialogue runner (a condition, a substitution): running out counts
/// as false.
pub const COND_FUEL: u64 = 500;

/// A timed line's duration: this many ticks, plus [`LINE_TICKS_PER_CHAR`] per character of its text.
pub const LINE_BASE_TICKS: u32 = 30;

/// A timed line's ticks per character of its source text.
pub const LINE_TICKS_PER_CHAR: u32 = 1;

/// The shortest derived duration of a timed line.
pub const LINE_MIN_TICKS: u32 = 30;

/// The longest derived duration of a timed line.
pub const LINE_MAX_TICKS: u32 = 200;

/// Steps of a program's dialogue graph (all its dialogues together).
pub const GRAPH_STEPS_MAX: usize = 65_535;

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
        assert_eq!(COND_FUEL, 500);
        assert_eq!(LINE_BASE_TICKS, 30);
        assert_eq!(LINE_TICKS_PER_CHAR, 1);
        assert_eq!(LINE_MIN_TICKS, 30);
        assert_eq!(LINE_MAX_TICKS, 200);
        assert_eq!(GRAPH_STEPS_MAX, 65_535);
    }
}
