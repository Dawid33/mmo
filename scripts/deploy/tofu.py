"""OpenTofu wrapper: init, saved plan, `show -json` summary and saved-plan apply.

Every apply consumes the exact file that was planned. No bare `apply`, no
`-auto-approve`, and no variables on apply — the plan carries them.
"""

from __future__ import annotations

import json
from dataclasses import dataclass, field
from pathlib import Path
from typing import Mapping

from deploy.runner import DeployError, Runner
from deploy.targets import Root, Target

REPO = Path(__file__).resolve().parents[2]
TOFU = "tofu"


@dataclass
class Plan:
    path: Path
    changed: bool
    creates: list[str] = field(default_factory=list)
    updates: list[str] = field(default_factory=list)
    deletes: list[str] = field(default_factory=list)
    replaces: list[str] = field(default_factory=list)

    @property
    def destructive(self) -> bool:
        """Delete or replace: --yes must not auto-approve these."""
        return bool(self.deletes or self.replaces)

    def summary(self) -> str:
        counts = (
            ("create", self.creates),
            ("update", self.updates),
            ("delete", self.deletes),
            ("replace", self.replaces),
        )
        parts = [f"{len(items)} {name}" for name, items in counts if items]
        return ", ".join(parts) if parts else "no changes"


def env_for(target: Target, root: Root) -> dict[str, str]:
    cache = REPO / "target/tofu-plugin-cache"
    cache.mkdir(parents=True, exist_ok=True)
    env = {"TF_PLUGIN_CACHE_DIR": str(cache)}
    if target.state == "local":
        # The twin's state lives next to this checkout, never in the root dir.
        env["TF_DATA_DIR"] = str(REPO / "target/twin" / root.name / ".terraform")
    return env


def chdir(target: Target, root: Root) -> str:
    return f"-chdir={target.env_dir}/{root.name}"


def init(r: Runner, target: Target, root: Root) -> None:
    args = [TOFU, chdir(target, root), "init", "-input=false"]
    if target.state == "local":
        args.append("-reconfigure")
    else:
        args += [
            "-backend-config=../backend.s3.tfbackend",
            f"-backend-config=key=orion/{root.name}.tfstate",
        ]
    r.run(args, env=env_for(target, root))


def plan_path(target: Target, root: Root) -> Path:
    path = REPO / "target/deploy" / target.name / f"{root.name}.tfplan"
    path.parent.mkdir(parents=True, exist_ok=True)
    return path


def plan(r: Runner, target: Target, root: Root, extra_vars: Mapping[str, str] | None) -> Plan:
    path = plan_path(target, root)
    args = [
        TOFU,
        chdir(target, root),
        "plan",
        "-input=false",
        "-detailed-exitcode",
        f"-out={path}",
    ]
    if target.state == "s3":
        # Every orion root declares kube_context (checked); pin it, never the
        # operator's current context.
        args.append(f"-var=kube_context={target.kube_context}")
    for name, value in (extra_vars or {}).items():
        args.append(f"-var={name}={value}")
    result = r.run(args, check=False, env=env_for(target, root))
    if result.rc == 1:
        raise DeployError(f"{root.name}: tofu plan failed\n{(result.err or result.out).strip()}")
    if result.rc not in (0, 2):
        raise DeployError(f"{root.name}: unexpected tofu plan exit {result.rc}\n{result.err.strip()}")
    shown = r.run(
        [TOFU, chdir(target, root), "show", "-json", str(path)],
        capture=True,
        env=env_for(target, root),
    )
    data = json.loads(shown.out)
    entry = Plan(path=path, changed=result.rc == 2)
    for change in data.get("resource_changes", []):
        actions = change.get("change", {}).get("actions", [])
        address = change.get("address", "?")
        if set(actions) == {"create", "delete"}:
            entry.replaces.append(address)
        elif actions == ["create"]:
            entry.creates.append(address)
        elif actions == ["update"]:
            entry.updates.append(address)
        elif actions == ["delete"]:
            entry.deletes.append(address)
    return entry


def apply(r: Runner, target: Target, root: Root, plan: Plan) -> None:
    r.run(
        [TOFU, chdir(target, root), "apply", "-input=false", str(plan.path)],
        env=env_for(target, root),
    )


def has_state(r: Runner, target: Target, root: Root) -> bool:
    result = r.run(
        [TOFU, chdir(target, root), "state", "list"],
        check=False,
        capture=True,
        env=env_for(target, root),
    )
    if result.rc != 0:
        detail = (result.err or result.out).strip()
        if "No state file was found" in detail:
            return False
        raise DeployError(
            f"{root.name}: cannot read OpenTofu state\n{detail}"
        )
    return bool(result.out.strip())
