"""The deploy root graph as data: one target per cluster, one Root per OpenTofu root.

The order of `roots` is the apply order: each root is placed after every root
whose state it reads. Adding a directory under deploy/envs/<target>/ without
placing it here is caught by preflight and by the graph test.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Literal


@dataclass(frozen=True)
class Root:
    name: str
    reads: tuple[str, ...] = ()
    needs_vpn: bool = False
    needs_tfvars: bool = False
    images: tuple[str, ...] = ()
    wait: bool = False
    guard: bool = False
    fresh_vars: dict[str, str] | None = None


@dataclass(frozen=True)
class Target:
    name: str
    kube_context: str
    state: Literal["s3", "local"]
    registry: Literal["ecr", "k3d"]
    roots: list[Root] = field(default_factory=list)
    world_ns: str = ""
    min_world_hosts: int = 1

    @property
    def world_namespace(self) -> str:
        return self.world_ns or ("world-twin" if self.name == "twin" else "world-errantry")

    @property
    def env_dir(self) -> str:
        return "deploy/envs/local" if self.name == "twin" else "deploy/envs/orion"


# --infra walks this list once, in order. `garage`'s second pass (after
# monitoring) is driven by its fresh_vars, not by a duplicate Root.
ORION = Target(
    name="orion",
    kube_context="orion",
    state="s3",
    registry="ecr",
    world_ns="world-errantry",
    roots=[
        Root("platform"),
        Root("dns"),
        Root("registry"),
        Root("garage", fresh_vars={"metrics": "false", "until_state_of": "monitoring"}),
        Root("garage-buckets", reads=("garage",), needs_vpn=True),
        Root("pg-management", reads=("garage-buckets",), wait=True),
        Root("keycloak", reads=("pg-management",)),
        Root("vpn-names", reads=("keycloak",)),
        Root("keycloak-realm", reads=("keycloak",), needs_vpn=True, needs_tfvars=True),
        Root("monitoring", reads=("garage-buckets", "keycloak-realm"), needs_tfvars=True),
        Root("picoshare"),
        Root("zulip", reads=("garage-buckets", "keycloak-realm"), needs_tfvars=True),
        Root("postgres", reads=("garage-buckets",), wait=True),
        Root("login", reads=("keycloak-realm", "pg-management", "registry"), images=("login",)),
        Root("site", reads=("keycloak-realm",)),
        Root("world", reads=("login", "postgres", "registry"), images=("server", "web"), guard=True),
        Root("panel", reads=("registry",), images=("panel",)),
    ],
)

TWIN = Target(
    name="twin",
    kube_context="k3d-mmo",
    state="local",
    registry="k3d",
    world_ns="world-twin",
    min_world_hosts=2,
    roots=[
        Root("platform"),
        Root("postgres", wait=True),
        Root("pg-management", wait=True),
        Root("login", reads=("pg-management",), images=("login",)),
        Root("world", images=("server", "web"), guard=True),
        Root("panel", images=("panel",)),
    ],
)

TARGETS = {"orion": ORION, "twin": TWIN}


def select(target: Target, only: list[str] | None) -> list[Root]:
    """Keep the requested roots, in graph order. `only=None` means all."""
    if not only:
        return list(target.roots)
    wanted = set(only)
    return [root for root in target.roots if root.name in wanted]


def valid_only_names(target: Target) -> list[str]:
    """Names accepted by --only: the roots (login/panel/world among them)."""
    return [root.name for root in target.roots]
