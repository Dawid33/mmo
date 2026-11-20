"""Walk the root graph for --infra: saved plans, confirmation and the guards."""

from __future__ import annotations

import json
from dataclasses import dataclass, field
from typing import Callable, Mapping

from deploy import tofu
from deploy.runner import DeployError, Runner
from deploy.targets import Root, Target

HOST_SELECTOR = "app.kubernetes.io/component=host"


@dataclass
class InfraReport:
    applied: list[str] = field(default_factory=list)
    unchanged: list[str] = field(default_factory=list)
    skipped: dict[str, str] = field(default_factory=dict)

    def summary(self) -> str:
        lines = [
            f"infra: applied: {', '.join(self.applied) or 'none'}",
            f"infra: unchanged: {', '.join(self.unchanged) or 'none'}",
        ]
        for name, reason in self.skipped.items():
            lines.append(f"infra: skipped {name}: {reason}")
        if "world" in self.applied:
            lines.append("next: just deploy --game")
        if "world" in self.skipped:
            lines.append("next: just world-start, then re-run just deploy --infra")
        return "\n".join(lines)


def kubectl(r: Runner, target: Target, *args: str):
    ns = target.world_namespace
    return r.run(
        ["kubectl", "--context", target.kube_context, "-n", ns, *args],
        check=False,
        capture=True,
    )


def world_guard(r: Runner, target: Target) -> str | None:
    """Why the world root must not be applied right now, or None."""
    marker = kubectl(r, target, "get", "configmap", "release-in-progress")
    if marker.rc == 0:
        return "release-in-progress ConfigMap exists (a world update is under way)"

    listing = kubectl(
        r, target, "get", "deploy", "-l",
        "app.kubernetes.io/component in (coordinator,fleet,toolbox,host)", "-o", "json",
    )
    try:
        items = json.loads(listing.out).get("items", []) if listing.rc == 0 else []
    except (ValueError, AttributeError):
        items = []
    if listing.rc != 0 or not items:
        return "cannot read the world Deployments (is the kube API reachable?)"

    images: list[str] = []
    for item in items:
        spec = item.get("spec", {})
        name = item.get("metadata", {}).get("name", "?")
        if name == "coordinator" and str(spec.get("replicas", "")) == "0":
            return "coordinator is at 0 replicas (the world is stopped)"
        containers = spec.get("template", {}).get("spec", {}).get("containers", [])
        image = next((c.get("image", "") for c in containers if c.get("image")), "")
        if image:
            images.append(image)
    if len(set(images)) > 1:
        return "mixed server images across the coordinator, fleet, toolbox and hosts (an interrupted update)"
    return None


def wait_databases(r: Runner, target: Target) -> None:
    """The CNPG operator creates the database after the apply returns."""
    r.run(
        [
            "kubectl", "--context", target.kube_context, "-n", "postgres",
            "wait", f"database/{target.world_namespace}",
            "--for=jsonpath={.status.applied}=true", "--timeout=300s",
        ]
    )


def run(
    r: Runner,
    target: Target,
    roots: list[Root],
    *,
    yes: bool,
    dry_run: bool,
    ask: Callable[[str], bool],
    ensure_image: Callable[[str], None],
) -> InfraReport:
    report = InfraReport()
    state: dict[str, bool] = {}
    monitoring_root = next((root for root in target.roots if root.name == "monitoring"), None)
    garage_root = next((root for root in target.roots if root.name == "garage"), None)
    garage_pending = False

    def has_state(root: Root) -> bool:
        if root.name not in state:
            state[root.name] = tofu.has_state(r, target, root)
        return state[root.name]

    def walk(root: Root, extra_vars: Mapping[str, str] | None = None, second_pass: bool = False) -> None:
        nonlocal garage_pending
        if root.guard:
            reason = world_guard(r, target)
            if reason:
                report.skipped[root.name] = reason
                print(f"{root.name}: skipped: {reason}")
                return

        tofu.init(r, target, root)
        if not has_state(root):
            for image in root.images:
                ensure_image(image)

        fresh_vars = root.fresh_vars
        if not second_pass and fresh_vars and fresh_vars.get("metrics") == "false" and monitoring_root:
            if not has_state(monitoring_root):
                extra_vars = {"metrics": "false"}
                garage_pending = True

        try:
            planned = tofu.plan(r, target, root, extra_vars)
        except DeployError as exc:
            if root.needs_vpn and any(hint in str(exc) for hint in ("dial", "connection refused", "no such host")):
                raise DeployError(f"{root.name}: VPN down? `just orion-vpn-status`\n{exc}") from exc
            raise

        if not planned.changed:
            report.unchanged.append(root.name)
            print(f"{root.name}: unchanged")
        else:
            print(f"{root.name}: {planned.summary()}")
            for address in planned.creates + planned.updates + planned.deletes + planned.replaces:
                print(f"    {address}")
            if dry_run:
                report.skipped[root.name] = "dry run (nothing applied; later plans may be stale)"
                print(f"{root.name}: dry run, not applying")
            else:
                approved = (yes and not planned.destructive) or ask(f"apply {root.name}? [y/N]")
                if not approved:
                    report.skipped[root.name] = "declined"
                    print(f"{root.name}: not applied")
                else:
                    tofu.apply(r, target, root, planned)
                    report.applied.append(root.name)
                    state[root.name] = True

        if root.wait and root.name in report.applied:
            wait_databases(r, target)

        # garage was planned with metrics=false only because monitoring had no
        # state; once it exists, re-plan and apply garage without that var.
        if monitoring_root and root.name == monitoring_root.name:
            if garage_pending and garage_root and (dry_run or has_state(monitoring_root)):
                walk(garage_root, {}, second_pass=True)
                garage_pending = False

    for root in roots:
        walk(root)
    print(report.summary())
    return report
