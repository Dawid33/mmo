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


def crossing(host_flip, correction_max, corrections, confirm_ms=40.0, from_host=0, to_host=None):
    to_host = float(host_flip) if to_host is None else to_host
    return {"host_flip": float(host_flip), "correction_max": correction_max, "corrections": float(corrections),
            "confirm_ms": confirm_ms, "from_host": float(from_host), "to_host": float(to_host)}


SERVER_OK = [
    "INFO [server] role: Host",
    "INFO [server] window=5.0s sessions=1 regions=4 parked=0 transfers_inflight=0 pending_arrivals=0 "
    "peer_unreceived=2 arrivals_despawned=0 items_destroyed=0 intent_replies_lost=0 | lod hit=1",
    "INFO [server] window=5.0s sessions=0 regions=4 parked=0 transfers_inflight=0 pending_arrivals=0 "
    "peer_unreceived=0 arrivals_despawned=0 items_destroyed=0 intent_replies_lost=0 | lod hit=1",
]
CLIENT_OK = [
    "INFO [client-state-hash] checked=12 mismatched=0 (server VOXEL_STATE_HASH_INTERVAL)",
    "INFO [perf] event=autopilot_done cruise_s=60 reversals=80 ready=true",
    "INFO [session] session ended: left by the player",
]
STACK_OK = [
    "dev-r3: login on http://127.0.0.1:8790 (x), coordinator on 127.0.0.1:6480 (y)",
    "dev-r3: host 0 on UDP 6466 (target/go-nogo-r3/stack/host-0.log)",
    "dev-r3: host 1 on UDP 6476 (target/go-nogo-r3/stack/host-1.log)",
    "dev-r3: world dev registered with host 0's login link (127.0.0.1:6490)",
]


# What dev_r3.sh prints after the recipe's TERM when everything stops in time.
STACK_STOP_OK = [
    "dev-r3: stopping the hosts (each saves a leave cut with the coordinator)",
    "dev-r3: waiting up to 45s for the hosts (pids 11 12); Ctrl-C again kills at once",
    "dev-r3: stopping the coordinator",
    "dev-r3: stopping the login service",
    "dev-r3: stopped",
]


def window_line(clock, unreceived=0):
    """A host's `[server]` counters line as simplelog writes it (UTC HH:MM:SS)."""
    return (f"{clock} [INFO] [server] window=5.0s sessions=1 regions=4 parked=0 transfers_inflight=0 "
            f"pending_arrivals=0 peer_unreceived={unreceived} arrivals_despawned=0 items_destroyed=0 "
            "intent_replies_lost=0 | lod hit=1")


def timed_host(first, last):
    """A host log with a window line every 5 s from second `first` to `last` of 12:00."""
    return ["12:00:00 [INFO] [server] role: Host"] + [
        window_line(f"12:{t // 60:02}:{t % 60:02}") for t in range(first, last + 1, 5)
    ]


def timed_client(done_at):
    """A client log (Bevy's RFC 3339 UTC stamps) whose shuttle ended at 12:MM:SS."""
    return [
        "2026-11-13T12:00:01.000000Z  INFO client::instance: [client-state-hash] checked=12 mismatched=0",
        f"2026-11-13T12:{done_at // 60:02}:{done_at % 60:02}.500000Z  INFO game::perf: [perf] "
        "event=autopilot_done cruise_s=60 reversals=80 ready=true",
    ]


class GoNoGo(unittest.TestCase):
    """Decision 13: host p95 <= 1.25 x thread p95 + 0.05, per judged metric."""

    def run_verdict(self, thread, host, clients=None, servers=None, aux=None, stack=None, stack_alive=True,
                    stack_exit=0, stack_after=None):
        clients = clients or {"thread run": CLIENT_OK, "host run": CLIENT_OK}
        servers = servers or {"host-0.log": SERVER_OK, "host-1.log": SERVER_OK}
        aux = aux or {"coordinator.log": ["INFO [server] role: coordinator"], "login.log": ["INFO listening"]}
        stack = STACK_OK if stack is None else stack
        stack_after = STACK_STOP_OK if stack_after is None else stack_after
        return crossing_summary.verdict(
            thread,
            host,
            crossing_summary.conservation_problems(clients, servers),
            crossing_summary.invalid_reasons(clients, servers, aux, stack, stack_alive, stack_exit, stack_after),
        )

    def good(self):
        return [crossing(0, 1.0, 2) for _ in range(30)], [crossing(1, 1.0, 2) for _ in range(30)]

    def test_go_when_host_flips_match_thread_flips(self):
        thread = [crossing(0, 1.0, 2) for _ in range(30)]
        # 1.25 x 1.0 + 0.05 = 1.30 and 1.25 x 2 + 0.05 = 2.55: at the limit is still GO.
        host = [crossing(1, 1.3, 2) for _ in range(30)]
        # Crossings of the other kind in either run are not counted.
        word, reasons, rows = self.run_verdict(thread + [crossing(1, 99.0, 99)], host + [crossing(0, 99.0, 99)])
        self.assertEqual(word, "GO", reasons)
        self.assertEqual([r[0] for r in rows], ["correction_max", "corrections", "confirm_ms"])

    def test_no_go_by_ratio(self):
        thread = [crossing(0, 1.0, 2) for _ in range(30)]
        host = [crossing(1, 1.0, 3) for _ in range(30)]  # corrections 3 > 2.55
        word, reasons, _ = self.run_verdict(thread, host)
        self.assertEqual(word, "NO-GO")
        self.assertTrue(any(r.startswith("corrections:") for r in reasons), reasons)

    def test_slack_lets_a_zero_thread_p95_pass_small_noise(self):
        thread = [crossing(0, 0.0, 0) for _ in range(30)]
        self.assertEqual(self.run_verdict(thread, [crossing(1, 0.05, 0) for _ in range(30)])[0], "GO")
        self.assertEqual(self.run_verdict(thread, [crossing(1, 0.06, 0) for _ in range(30)])[0], "NO-GO")

    def test_no_go_by_conservation_whatever_the_timing(self):
        same = [crossing(0, 1.0, 2) for _ in range(30)], [crossing(1, 1.0, 2) for _ in range(30)]
        lost = SERVER_OK[:2] + [SERVER_OK[2].replace("intent_replies_lost=0", "intent_replies_lost=1")]
        word, reasons, _ = self.run_verdict(*same, servers={"host-0.log": SERVER_OK, "host-1.log": lost})
        self.assertEqual(word, "NO-GO")
        self.assertIn("intent_replies_lost", reasons[0])
        unreceived = SERVER_OK[:2] + [SERVER_OK[2].replace("peer_unreceived=0", "peer_unreceived=3")]
        self.assertEqual(self.run_verdict(*same, servers={"host-0.log": unreceived})[0], "NO-GO")
        destroyed = [SERVER_OK[0], SERVER_OK[1].replace("items_destroyed=0", "items_destroyed=1"), SERVER_OK[2]]
        self.assertEqual(self.run_verdict(*same, servers={"host-0.log": destroyed})[0], "NO-GO")
        mismatch = [CLIENT_OK[0].replace("mismatched=0", "mismatched=1")] + CLIENT_OK[1:]
        word, _, _ = self.run_verdict(*same, clients={"thread run": CLIENT_OK, "host run": mismatch})
        self.assertEqual(word, "NO-GO")
        # peer_unreceived above 0 mid-run is fine; only the last two lines count.
        self.assertEqual(self.run_verdict(*same)[0], "GO")

    def test_peer_unreceived_fails_only_on_the_last_two_lines(self):
        line = SERVER_OK[2]
        tail = lambda a, b: [SERVER_OK[0], line.replace("peer_unreceived=0", f"peer_unreceived={a}"),  # noqa: E731
                             line.replace("peer_unreceived=0", f"peer_unreceived={b}")]
        # Frames in flight at the last sample (a cut's mark) only: not a loss.
        self.assertEqual(self.run_verdict(*self.good(), servers={"host-0.log": tail(0, 4)})[0], "GO")
        self.assertEqual(self.run_verdict(*self.good(), servers={"host-0.log": tail(3, 0)})[0], "GO")
        word, reasons, _ = self.run_verdict(*self.good(), servers={"host-0.log": tail(3, 4)})
        self.assertEqual(word, "NO-GO")
        self.assertIn("peer_unreceived", reasons[0])

    def test_unknown_sides_are_never_grouped(self):
        thread, host = self.good()
        # host_flip=1 with one side unknown (-1): counted apart, not a host flip.
        unknown = [crossing(1, 99.0, 99, from_host=-1, to_host=1) for _ in range(30)]
        word, reasons, _ = self.run_verdict(thread, host + unknown)
        self.assertEqual(word, "GO", reasons)
        # A host run whose only "host flips" have an unknown side has no host group.
        word, reasons, _ = self.run_verdict(thread, unknown)
        self.assertEqual(word, "INVALID")
        self.assertIn("host seam", reasons[0])
        # A thread run's crossing between two known, different hosts is not a thread flip.
        self.assertEqual(self.run_verdict([crossing(0, 1.0, 2, from_host=0, to_host=1)] * 30, host)[0], "INVALID")
        groups = crossing_summary.seam_groups(thread + unknown, host + unknown)
        self.assertEqual((len(groups["thread"]), len(groups["host"]), groups["unknown"]), (30, 30, 60))

    def test_invalid(self):
        good = [crossing(0, 1.0, 2) for _ in range(30)], [crossing(1, 1.0, 2) for _ in range(30)]
        # Too few crossings in a group.
        word, reasons, _ = self.run_verdict(good[0], good[1][:19])
        self.assertEqual(word, "INVALID")
        self.assertIn("host seam", reasons[0])
        # A host restarted during the run.
        restarted = SERVER_OK + ["INFO [server] role: Host"]
        self.assertEqual(self.run_verdict(*good, servers={"host-0.log": restarted})[0], "INVALID")
        # No state-hash check ran in a client.
        word, _, _ = self.run_verdict(*good, clients={"thread run": CLIENT_OK, "host run": ["nothing"]})
        self.assertEqual(word, "INVALID")
        # A client panicked.
        panicked = CLIENT_OK + ["thread 'main' panicked at src/x.rs:1:1"]
        self.assertEqual(self.run_verdict(*good, clients={"thread run": panicked, "host run": CLIENT_OK})[0], "INVALID")

    def test_invalid_when_a_host_panicked(self):
        panicked = SERVER_OK[:2] + ["thread 'region (3,4)' panicked at crates/game/src/x.rs:1:1:"] + SERVER_OK[2:]
        word, reasons, _ = self.run_verdict(*self.good(), servers={"host-0.log": SERVER_OK, "host-1.log": panicked})
        self.assertEqual(word, "INVALID")
        self.assertIn("host-1.log", reasons[0])
        # The coordinator or the login service too.
        aux = {"coordinator.log": ["thread 'main' panicked at crates/server/src/lib.rs:9:9:"], "login.log": []}
        self.assertEqual(self.run_verdict(*self.good(), aux=aux)[0], "INVALID")

    def test_invalid_when_the_stack_ended_early(self):
        # Host 1 fenced (exit 3) mid-run: the supervisor takes the stack down.
        ended = STACK_OK + ["dev-r3: host 1 exited 3; see target/go-nogo-r3/stack/host-1.log",
                            "dev-r3: host 1 (target/go-nogo-r3/stack/host-1.log) ended (3): stopping the stack",
                            "dev-r3: stopped (failed)"]
        word, reasons, _ = self.run_verdict(*self.good(), stack=ended, stack_alive=False)
        self.assertEqual(word, "INVALID")
        self.assertTrue(any("exited 3" in r for r in reasons), reasons)
        # The supervisor gone at stop time is enough on its own.
        self.assertEqual(self.run_verdict(*self.good(), stack_alive=False)[0], "INVALID")
        # A restart that gave up.
        gave_up = STACK_OK + ["dev-r3: host 0 exited 5 after 3 restarts within 60s: giving up; see x"]
        self.assertEqual(self.run_verdict(*self.good(), stack=gave_up)[0], "INVALID")

    def test_invalid_when_the_stack_did_not_stop_cleanly(self):
        # A hung host: the supervisor KILLed it at the stop and exited 1.
        word, reasons, _ = self.run_verdict(*self.good(), stack_exit=1)
        self.assertEqual(word, "INVALID")
        self.assertTrue(any("did not stop cleanly" in r for r in reasons), reasons)
        killed = STACK_STOP_OK[:2] + ["dev-r3: killing the hosts (pids 12)", "dev-r3: stopped (failed)"]
        word, reasons, _ = self.run_verdict(*self.good(), stack_after=killed)
        self.assertEqual(word, "INVALID")
        self.assertTrue(any("killing the hosts" in r for r in reasons), reasons)
        unconfirmed = STACK_STOP_OK[:2] + ["dev-r3: could not confirm the hosts gone (pids 12)"]
        self.assertEqual(self.run_verdict(*self.good(), stack_after=unconfirmed)[0], "INVALID")
        # The same wordings before the stop count too.
        before = STACK_OK + ["dev-r3: killing the coordinator (pids 9)"]
        self.assertEqual(self.run_verdict(*self.good(), stack=before)[0], "INVALID")

    def test_invalid_when_a_host_stops_reporting_before_the_end(self):
        clients = {"thread run": timed_client(70), "host run": timed_client(160)}
        # Both hosts report until the host-seam shuttle ends: GO.
        whole = {"host-0.log": timed_host(5, 165), "host-1.log": timed_host(5, 160)}
        word, reasons, _ = self.run_verdict(*self.good(), clients=clients, servers=whole)
        self.assertEqual(word, "GO", reasons)
        # Host 1 hung at 12:01:40, a minute before the end; its last lines were clean.
        hung = {"host-0.log": timed_host(5, 165), "host-1.log": timed_host(5, 100)}
        word, reasons, _ = self.run_verdict(*self.good(), clients=clients, servers=hung)
        self.assertEqual(word, "INVALID")
        self.assertIn("host-1.log", reasons[0])
        # Midnight between the last window line and the end of the shuttle is no gap.
        late = {"thread run": ["2026-11-13T23:59:58.000000Z  INFO x: [client-state-hash] checked=1 mismatched=0",
                               "2026-11-14T00:00:03.000000Z  INFO x: [perf] event=autopilot_done cruise_s=60"]}
        late["host run"] = late["thread run"]
        night = {"host-0.log": [window_line("23:59:59")], "host-1.log": [window_line("00:00:01")]}
        self.assertEqual(self.run_verdict(*self.good(), clients=late, servers=night)[0], "GO")

    def test_without_timestamps_a_host_far_behind_its_peer_is_invalid(self):
        many = ["INFO [server] role: Host"] + [SERVER_OK[2]] * 30
        few = ["INFO [server] role: Host"] + [SERVER_OK[2]] * 5
        word, reasons, _ = self.run_verdict(*self.good(), servers={"host-0.log": many, "host-1.log": few})
        self.assertEqual(word, "INVALID")
        self.assertIn("host-1.log", reasons[0])
        self.assertEqual(self.run_verdict(*self.good(), servers={"host-0.log": many, "host-1.log": many})[0], "GO")

    def test_invalid_when_one_host_log_has_no_window_line(self):
        word, reasons, _ = self.run_verdict(*self.good(), servers={"host-0.log": SERVER_OK,
                                                                   "host-1.log": SERVER_OK[:1]})
        self.assertEqual(word, "INVALID")
        self.assertIn("host-1.log", reasons[0])

    def test_invalid_when_a_client_lost_its_session_early(self):
        lost = [CLIENT_OK[0], "ERROR Server connection closed: ConnectionLost",
                "INFO [session] session ended: connection lost", CLIENT_OK[1]]
        word, reasons, _ = self.run_verdict(*self.good(), clients={"thread run": CLIENT_OK, "host run": lost})
        self.assertEqual(word, "INVALID")
        self.assertIn("host run", reasons[0])
        # The shuttle never ran its time (no autopilot_done: killed by the recipe's timeout).
        short = CLIENT_OK[:1]
        self.assertEqual(self.run_verdict(*self.good(), clients={"thread run": short, "host run": CLIENT_OK})[0],
                         "INVALID")
        # A refusal by a host.
        refused = [CLIENT_OK[0], "ERROR [client] the server refused the connection: full", CLIENT_OK[1]]
        self.assertEqual(self.run_verdict(*self.good(), clients={"thread run": refused, "host run": CLIENT_OK})[0],
                         "INVALID")

    def test_parsed_lines_feed_the_verdict(self):
        thread = crossing_summary.crossing_lines([LINES[0]] * 25)
        host = crossing_summary.crossing_lines([LINES[1]] * 25)
        word, reasons, rows = self.run_verdict(thread, host)
        # correction_max 2.25 > 1.25 x 0.5 + 0.05.
        self.assertEqual(word, "NO-GO", reasons)
        self.assertEqual(rows[0][1:3], (0.5, 2.25))


if __name__ == "__main__":
    unittest.main()
