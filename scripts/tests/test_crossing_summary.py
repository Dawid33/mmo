"""crossing_summary.py's grouping of the game thread's region crossings
(`python3 -m unittest discover -s scripts/tests -p "test_crossing_summary.py"`)."""
import os
import sys
import unittest

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))

import crossing_summary  # noqa: E402

LINES = [
    "[perf] event=region_crossing from=0,0 to=1,0 window_ms=2000.0 confirm_ms=40.0 stalled_at_flip=0 "
    "tick_gap_max_ms=50.0 before_tick_gap_max_ms=50.0 home_stalled_ticks=0 home_align=0(+0t) corrections=1 "
    "correction_max=0.50u snapshots=3 snapshot_ms=9.0 snapshot_max_ms=4.0 host_flip=0 from_host=0 to_host=0",
    "[perf] event=region_crossing from=1,0 to=2,0 window_ms=2000.0 confirm_ms=120.0 stalled_at_flip=1 "
    "tick_gap_max_ms=60.0 before_tick_gap_max_ms=50.0 home_stalled_ticks=1 home_align=1(-1t) corrections=4 "
    "correction_max=2.25u snapshots=3 snapshot_ms=9.0 snapshot_max_ms=4.0 host_flip=1 from_host=0 to_host=1",
    "\x1b[32m[perf] event=region_crossing from=2,0 to=3,0 window_ms=2000.0 confirm_ms=-1.0 stalled_at_flip=0 "
    "tick_gap_max_ms=55.0 before_tick_gap_max_ms=50.0 home_stalled_ticks=0 home_align=0(+0t) corrections=2 "
    "correction_max=1.00u snapshots=3 snapshot_ms=9.0 snapshot_max_ms=4.0 host_flip=1 from_host=1 to_host=0\x1b[0m",
    "[perf] event=region_crossing_render after: frame_max_ms=12.0 | before: frame_max_ms=10.0",
]


class GroupsByHostFlip(unittest.TestCase):
    def test_groups_by_host_flip(self):
        crossings = crossing_summary.crossing_lines(LINES)
        self.assertEqual(len(crossings), 3, "the render line is not a game-thread crossing")
        groups = crossing_summary.p95_by(crossings, "host_flip")
        self.assertEqual(sorted(groups), ["0", "1"])
        self.assertEqual(groups["0"], {"correction_max": 0.5, "corrections": 1.0, "confirm_ms": 40.0})
        # Two host flips: the p95 of two values is the larger; a crossing never
        # confirmed (confirm_ms=-1) does not count.
        self.assertEqual(groups["1"], {"correction_max": 2.25, "corrections": 4.0, "confirm_ms": 120.0})

    def test_without_a_key_one_group(self):
        groups = crossing_summary.p95_by(crossing_summary.crossing_lines(LINES))
        self.assertEqual(list(groups), ["all"])
        self.assertEqual(groups["all"]["corrections"], 4.0)


if __name__ == "__main__":
    unittest.main()
