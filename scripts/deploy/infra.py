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
    guarded: set[str] = field(default_factory=set)
    fresh_applied: set[str] = field(default_factory=set)

    def summary(self) -> str:
        lines = [
            f"infra: applied: {', '.join(self.applied) or 'none'}",
            f"infra: unchanged: {', '.join(self.unchanged) or 'none'}",
        ]
        for name, reason in self.skipped.items():
            lines.append(f"infra: skipped {name}: {reason}")
        if "world" in self.fresh_applied:
            lines.append("next: just deploy --game")
        if "world" in self.guarded:
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
    print("infra: world: checking release marker and running workload images", flush=True)
    marker = kubectl(
        r, target, "get", "configmap", "release-in-progress",
        "--ignore-not-found", "-o", "name",
    )
    if marker.rc != 0:
        raise DeployError(
            f"cannot check the world release marker: "
            f"{(marker.err or marker.out).strip() or f'exit {marker.rc}'}"
        )
    if marker.out.strip():
        return "release-in-progress ConfigMap exists (a world update is under way)"

    listing = kubectl(
        r, target, "get", "deploy", "-l",
        "app.kubernetes.io/component in (coordinator,fleet,toolbox,host)", "-o", "json",
    )
    if listing.rc != 0:
        raise DeployError(
            f"cannot read the world Deployments: "
            f"{(listing.err or listing.out).strip() or f'exit {listing.rc}'}"
        )
    try:
        items = json.loads(listing.out).get("items", [])
    except (ValueError, AttributeError) as exc:
        raise DeployError("kubectl returned invalid world Deployment JSON") from exc

    images: list[str] = []
    for item in items:
        spec = item.get("spec", {})
        name = item.get("metadata", {}).get("name", "?")
        if name == "coordinator" and str(spec.get("replicas", "")) == "0":
            return "coordinator is at 0 replicas (the world is stopped)"
        containers = spec.get("template", {}).get("spec", {}).get("containers", [])
        want = {
            "coordinator": "coordinator",
            "fleet": "fleet",
            "toolbox": "toolbox",
        }.get(name, "host" if name.startswith("host-") else "")
        image = next(
            (c.get("image", "") for c in containers if c.get("name") == want and c.get("image")),
            "",
        )
        if image:
            images.append(image)
    if len(set(images)) > 1:
        return "mixed server images across the coordinator, fleet, toolbox and hosts (an interrupted update)"
    return None


def wait_databases(r: Runner, target: Target, root: Root) -> None:
    """The CNPG operator creates the database after the apply returns."""
    if root.name == "pg-management":
        namespace = "pg-management"
        databases = ("login", "keycloak")
        resource = "management"
    else:
        namespace = "postgres"
        databases = (target.world_namespace,)
        resource = "world"
    print(
        f"infra: {root.name}: waiting for every {resource} Database CR to be applied",
        flush=True,
    )
    r.run(
        [
            "kubectl", "--context", target.kube_context, "-n", namespace,
            "wait", *[f"database/{database}" for database in databases],
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
    initialized: set[str] = set()
    monitoring_root = next((root for root in target.roots if root.name == "monitoring"), None)
    garage_root = next((root for root in target.roots if root.name == "garage"), None)
    garage_pending = False
    selected = {root.name for root in roots}

    def init_root(root: Root) -> None:
        if root.name not in initialized:
            print(f"infra: {root.name}: initializing OpenTofu", flush=True)
            tofu.init(r, target, root)
            initialized.add(root.name)

    def has_state(root: Root) -> bool:
        init_root(root)
        print(f"infra: {root.name}: reading existing state", flush=True)
        if root.name not in state:
            state[root.name] = tofu.has_state(r, target, root)
        return state[root.name]

    def walk(root: Root, extra_vars: Mapping[str, str] | None = None, second_pass: bool = False) -> None:
        nonlocal garage_pending
        pass_name = " (second pass)" if second_pass else ""
        print(f"infra: {root.name}{pass_name}: starting", flush=True)
        init_root(root)
        existing = has_state(root)
        if root.guard:
            reason = world_guard(r, target)
            if reason:
                report.skipped[root.name] = reason
                report.guarded.add(root.name)
                print(f"infra: {root.name}: skipped: {reason}", flush=True)
                return

        if not existing and root.images and not dry_run:
            for image in root.images:
                print(f"infra: {root.name}: ensuring bootstrap image {image}", flush=True)
                ensure_image(image)
                print(f"infra: {root.name}: bootstrap image {image} available", flush=True)

        fresh_vars = root.fresh_vars
        if not second_pass and fresh_vars and fresh_vars.get("metrics") == "false" and monitoring_root:
            if not has_state(monitoring_root):
                extra_vars = {"metrics": "false"}
                garage_pending = True
                print("infra: garage: monitoring has no state; planning with metrics=false", flush=True)

        print(f"infra: {root.name}{pass_name}: planning saved OpenTofu plan", flush=True)
        try:
            planned = tofu.plan(r, target, root, extra_vars)
        except DeployError as exc:
            if root.needs_vpn and any(hint in str(exc) for hint in ("dial", "connection refused", "no such host")):
                raise DeployError(f"{root.name}: VPN down? `just orion-vpn-status`\n{exc}") from exc
            raise

        if not planned.changed:
            report.unchanged.append(root.name)
            print(f"infra: {root.name}: plan unchanged", flush=True)
        else:
            print(f"infra: {root.name}: plan changed: {planned.summary()}", flush=True)
            for address in planned.creates + planned.updates + planned.deletes + planned.replaces:
                print(f"    {address}")
            if dry_run:
                report.skipped[root.name] = "dry run (nothing applied; later plans may be stale)"
                print(f"infra: {root.name}: dry run, not applying", flush=True)
            else:
                approved = (yes and not planned.destructive) or ask(f"apply {root.name}? [y/N]")
                if not approved:
                    report.skipped[root.name] = "declined"
                    print(f"infra: {root.name}: not applied (declined)", flush=True)
                else:
                    print(f"infra: {root.name}: applying saved plan", flush=True)
                    tofu.apply(r, target, root, planned)
                    if root.name not in report.applied:
                        report.applied.append(root.name)
                    if not existing:
                        report.fresh_applied.add(root.name)
                    state[root.name] = True
                    print(f"infra: {root.name}: apply complete", flush=True)

        if root.wait and root.name in report.applied:
            wait_databases(r, target, root)

        # garage was planned with metrics=false only because monitoring had no
        # state; once it exists, re-plan and apply garage without that var.
        if monitoring_root and root.name == monitoring_root.name:
            if (
                garage_pending
                and garage_root
                and garage_root.name in selected
                and (dry_run or has_state(monitoring_root))
            ):
                walk(garage_root, {}, second_pass=True)
                garage_pending = False

    print(f"infra: walking {len(roots)} selected root(s)", flush=True)
    for index, root in enumerate(roots, start=1):
        print(f"infra: progress {index}/{len(roots)}: {root.name}", flush=True)
        walk(root)
    print(report.summary(), flush=True)
    return report
