#!/usr/bin/env python3
"""Reduce a folded perf profile to pasteable top-N tables.

`just perf-client` / `just perf-server` write `perf-folded-{client,server}.txt`: one line
per unique stack, `thread;outermost;...;innermost <samples>` (inferno-collapse-perf). That
file runs to tens of MB, so it is useless to read directly; this collapses it into "which
functions cost the most", per thread and per crate.

    scripts/perf_summary.py perf-folded-client.txt [--top N]
    scripts/perf_summary.py perf-folded-client.txt --thread game     # one thread only
    scripts/perf_summary.py perf-folded-client.txt --prefix rapier3d:: --prefix parry3d::

Two numbers per frame, both as a share of the samples that survived the filters:
  * self  — samples where the frame was the innermost one: time spent IN the function.
  * incl  — samples with the frame anywhere on the stack: time spent in it or below it.
            A recursive frame is counted once per stack, so incl never exceeds 100%.

`--thread` is a substring match on the first stack element (the thread's comm, truncated
to 15 chars by the kernel). `--prefix` keeps only table rows whose symbol starts with one
of the given prefixes (`game::`, `client::`, `rapier3d::`, `bevy_`, ...) — the percentages
stay relative to all filtered samples, so "rapier3d:: is 40% incl" reads the same with or
without it. Generic wrappers hide the crate (`<T as core::ops::FnOnce>::call_once`), so
prefix matching also looks inside a leading `<`.
"""

import argparse
import collections
import sys


def matches(symbol: str, prefixes: list[str]) -> bool:
    if not prefixes:
        return True
    bare = symbol.lstrip("<&")
    return any(symbol.startswith(p) or bare.startswith(p) for p in prefixes)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("folded", help="folded stacks from inferno-collapse-perf")
    ap.add_argument("--top", type=int, default=30, help="rows per table (default 30)")
    ap.add_argument("--thread", action="append", default=[],
                    help="keep only stacks whose thread name contains this (repeatable)")
    ap.add_argument("--prefix", action="append", default=[],
                    help="keep only rows whose symbol starts with this (repeatable)")
    ap.add_argument("--width", type=int, default=110, help="truncate symbols to this many chars")
    args = ap.parse_args()

    threads: collections.Counter = collections.Counter()
    self_samples: collections.Counter = collections.Counter()
    incl_samples: collections.Counter = collections.Counter()
    crates: collections.Counter = collections.Counter()
    total = 0
    all_samples = 0

    with open(args.folded, errors="replace") as f:
        for line in f:
            stack, _, count = line.rstrip().rpartition(" ")
            if not stack or not count.isdigit():
                continue
            n = int(count)
            frames = stack.split(";")
            thread = frames[0]
            all_samples += n
            if args.thread and not any(t in thread for t in args.thread):
                continue
            total += n
            threads[thread] += n
            if len(frames) < 2:
                continue
            self_samples[frames[-1]] += n
            for frame in set(frames[1:]):
                incl_samples[frame] += n
            # Crate of the innermost frame = where the CPU actually was.
            leaf = frames[-1].lstrip("<&")
            crates[leaf.split("::", 1)[0] if "::" in leaf else "(no crate: libc/kernel/unknown)"] += n

    if not total:
        print(f"no samples in {args.folded} matched — check --thread against the thread table "
              f"(run without --thread first)", file=sys.stderr)
        return 1

    def pct(n: int) -> str:
        return f"{100.0 * n / total:5.1f}%"

    def clip(s: str) -> str:
        return s if len(s) <= args.width else s[: args.width - 1] + "…"

    print(f"{args.folded}: {all_samples} samples, {total} after filters"
          f"{f' (thread~{args.thread})' if args.thread else ''}\n")

    print(f"{'samples':>9} {'share':>6}  thread")
    print("-" * 40)
    for name, n in threads.most_common(args.top):
        print(f"{n:9d} {pct(n)}  {name}")

    print(f"\n{'samples':>9} {'self':>6}  crate of the innermost frame")
    print("-" * 60)
    for name, n in crates.most_common(args.top):
        print(f"{n:9d} {pct(n)}  {name}")

    for title, table in (("self", self_samples), ("incl", incl_samples)):
        print(f"\n{'samples':>9} {title:>6}  symbol"
              f"{f'  (prefix {args.prefix})' if args.prefix else ''}")
        print("-" * 100)
        rows = [(name, n) for name, n in table.most_common() if matches(name, args.prefix)]
        for name, n in rows[: args.top]:
            print(f"{n:9d} {pct(n)}  {clip(name)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
