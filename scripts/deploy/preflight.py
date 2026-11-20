"""Preflight: stop before the first plan when a run cannot possibly succeed."""

from __future__ import annotations

import shutil
from pathlib import Path

from deploy.runner import Runner
from deploy.targets import Root, Target

REPO = Path(__file__).resolve().parents[2]


def check(r: Runner, target: Target, roots: list[Root]) -> list[str]:
    """Return problems; an empty list means go ahead."""
    problems: list[str] = []

    tools = {"tofu", "kubectl", "docker", "just", "flock"}
    if target.state == "s3" or target.registry == "ecr":
        tools.add("aws")
    for tool in sorted(tools):
        if shutil.which(tool) is None:
            problems.append(f"missing tool: {tool}")

    # A new root directory must be placed in the graph before --infra runs.
    env_dir = REPO / target.env_dir
    if env_dir.is_dir():
        dirs = {p.name for p in env_dir.iterdir() if p.is_dir()} - {"bootstrap"}
        known = {root.name for root in target.roots}
        for name in sorted(dirs - known):
            problems.append(
                f"unplaced root dir {target.env_dir}/{name}: add it to the root graph in scripts/deploy/targets.py"
            )

    if target.state == "s3":
        identity = r.run(["aws", "sts", "get-caller-identity"], check=False, capture=True)
        if identity.rc != 0:
            detail = (identity.err or identity.out).strip().splitlines()
            problems.append(
                "AWS identity failed (expired SSO credentials?): " + (detail[0] if detail else f"exit {identity.rc}")
            )

    live = r.run(
        ["kubectl", "--context", target.kube_context, "get", "--raw", "/livez"],
        check=False,
        capture=True,
    )
    if live.rc != 0:
        problems.append(f"kube API {target.kube_context} is unreachable: {live.err.strip() or f'exit {live.rc}'}")

    if any(root.needs_vpn for root in roots):
        vpn = r.run(
            ["getent", "hosts", "kube-dns.kube-system.svc.cluster.local"],
            check=False,
            capture=True,
        )
        if vpn.rc != 0:
            problems.append("VPN down? `just orion-vpn-status` (or `just orion-vpn-up`) is needed by the selected roots")

    for root in roots:
        if root.needs_tfvars and not (env_dir / root.name / "terraform.tfvars").is_file():
            problems.append(f"missing {target.env_dir}/{root.name}/terraform.tfvars (a secret, never in git)")

    return problems
