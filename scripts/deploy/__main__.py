"""`just deploy --infra [--game]` entry point (PYTHONPATH=scripts python3 -m deploy)."""

from __future__ import annotations

import argparse
from pathlib import Path

from deploy import game, images, infra, preflight
from deploy.runner import DeployError, Runner
from deploy.targets import TARGETS, select, valid_only_names


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="deploy",
        description="Bring a cluster up to date: --infra (OpenTofu roots) and/or --game (images and release).",
    )
    parser.add_argument("--infra", action="store_true", help="plan and apply every OpenTofu root in order")
    parser.add_argument("--game", action="store_true", help="build, push and release login, world and panel")
    parser.add_argument("--target", choices=sorted(TARGETS), default="orion")
    parser.add_argument("--yes", action="store_true", help="skip confirmation for create/update-only changes")
    parser.add_argument("--dry-run", action="store_true", help="plan everything, apply nothing")
    parser.add_argument("--only", action="append", default=[], metavar="ROOT", help="limit the run (repeatable)")
    parser.add_argument("--forge-preset", default="earth-like", help="preset for a first forge")
    parser.add_argument("--push-only", action="store_true", help=argparse.SUPPRESS)  # twin-images alias
    return parser


def ask(question: str) -> bool:
    try:
        answer = input(f"{question} ").strip().lower()
    except EOFError:
        return False
    return answer in ("y", "yes")


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    target = TARGETS[args.target]
    valid = valid_only_names(target)
    bad = [name for name in args.only if name not in valid]
    if bad:
        print(f"deploy: unknown --only name(s): {', '.join(bad)}; valid: {', '.join(valid)}")
        return 2
    if not args.infra and not args.game:
        print("deploy: pick a mode: --infra and/or --game")
        return 2

    r = Runner(cwd=Path(__file__).resolve().parents[2])
    roots = select(target, args.only)
    modes = " + ".join(name for name, enabled in (("infra", args.infra), ("game", args.game)) if enabled)
    selected = ", ".join(root.name for root in roots) if args.infra else "game components only"
    print(
        f"deploy: starting target={target.name} modes={modes} dry_run={args.dry_run} yes={args.yes}",
        flush=True,
    )
    print(f"deploy: selection: {selected}", flush=True)
    problems = preflight.check(r, target, roots if args.infra else [])
    if problems:
        print("deploy: preflight failed:")
        for problem in problems:
            print(f"  - {problem}")
        return 2

    infra_report = None
    try:
        if args.infra:
            print(
                f"deploy: infra phase: {len(roots)} root(s), "
                f"{'dry run' if args.dry_run else 'plan then apply'}",
                flush=True,
            )
            infra_report = infra.run(
                r, target, roots, yes=args.yes, dry_run=args.dry_run,
                ask=ask,
                ensure_image=lambda name: images.ensure_latest(r, target, name),
            )
            if (
                args.game
                and infra_report
                and "world" in infra_report.guarded
                and (not args.only or "world" in args.only)
            ):
                print("deploy: stopping before game: world infra guard skipped a requested root", flush=True)
                return 3
            print("deploy: infra phase: complete", flush=True)
        if args.game:
            components = set(args.only) & {"login", "world", "panel"} if args.only else {
                "login", "world", "panel"
            }
            if components:
                print(
                    "deploy: game phase: " + ", ".join(sorted(components)),
                    flush=True,
                )
                game.run(
                    r, target, components, yes=args.yes, dry_run=args.dry_run,
                    ask=ask, forge_preset=args.forge_preset, push_only=args.push_only,
                )
                print("deploy: game phase: complete", flush=True)
    except DeployError as exc:
        print(f"deploy: stopped: {exc}", flush=True)
        return exc.code

    # A guard that skipped a specifically requested root means the run did not
    # do what was asked, even if the rest of the walk succeeded.
    if infra_report and "world" in infra_report.guarded and (not args.only or "world" in args.only):
        print("deploy: finished with a requested world root guarded", flush=True)
        return 3
    print("deploy: finished successfully", flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
