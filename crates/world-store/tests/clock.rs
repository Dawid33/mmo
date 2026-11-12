//! The world row's clock anchor: first writer wins, `clear` lets the next one in.

use game::ClockAnchor;

use crate::handle::{each_backend, open};

#[test]
fn first_anchor_wins() {
    each_backend("clock", false, |url| {
        let h = open(url);
        let first = ClockAnchor { unix_ms: 1_700_000_000_000, tick: 42 };
        let second = ClockAnchor { unix_ms: 1_700_000_999_000, tick: 7 };
        assert_eq!(h.ensure_clock_anchor(first).unwrap(), first);
        assert_eq!(h.ensure_clock_anchor(second).unwrap(), first, "the first proposal stays");
        h.clear_clock_anchor().unwrap();
        assert_eq!(h.ensure_clock_anchor(second).unwrap(), second, "a cleared anchor takes the next proposal");
        assert_eq!(h.ensure_clock_anchor(first).unwrap(), second);
        h.shutdown();
    });
}
