"""--game: build, push and release login, world and panel, then forge/register.

A release is idempotent by *content*, not by digest: the image label
`mmo.content-sha256` is compared with the hash recorded on the workload. The
world is only restarted when its server or web content actually changed.
"""

from __future__ import annotations

import argparse
import base64
import hashlib
import json
import shlex
import time
from dataclasses import dataclass, field
from pathlib import Path

from deploy import images
from deploy.runner import DeployError, Runner
from deploy.targets import ORION, TARGETS, Target

HOST_SELECTOR = "app.kubernetes.io/component=host"
FORGE_SH = 'exec worldctl forge --db "$VOXEL_WORLD_DB" "$@"'


@dataclass
class GameReport:
    released: list[str] = field(default_factory=list)
    unchanged: list[str] = field(default_factory=list)
    skipped: dict[str, str] = field(default_factory=dict)
    pushed: list[str] = field(default_factory=list)
    forged: bool = False
    registered: bool = False

    def summary(self) -> str:
        lines = [
            "game: released: " + (", ".join(self.released) or "none"),
            "game: unchanged: " + (", ".join(self.unchanged) or "none"),
        ]
        for name, reason in self.skipped.items():
            lines.append(f"game: skipped {name}: {reason}")
        if self.pushed:
            lines.append("game: pushed only (no release): " + ", ".join(self.pushed))
        if self.forged:
            lines.append("game: forged the world")
        if self.registered:
            lines.append("game: registered the world with login")
        return "\n".join(lines)


def kubectl(r: Runner, target: Target, *args: str, check: bool = False, capture: bool = True):
    return r.run(
        ["kubectl", "--context", target.kube_context, "-n", target.world_namespace, *args],
        check=check, capture=capture,
    )


def _json(response) -> dict:
    try:
        return json.loads(response.out) if response.out else {}
    except ValueError:
        return {}


def running_ref(r: Runner, target: Target, kind: str) -> str:
    """The ref currently running on coordinator (server) or web."""
    name = "coordinator" if kind == "server" else "web"
    response = kubectl(r, target, "get", f"deploy/{name}", "-o", "json")
    containers = _json(response).get("spec", {}).get("template", {}).get("spec", {}).get("containers", [])
    image = next((c.get("image", "") for c in containers if c.get("image")), "")
    if not image:
        raise DeployError(f"cannot read the running {kind} image from deploy/{name}")
    return image


def release_simple(r: Runner, target: Target, name: str, ref: str, content: str) -> None:
    """login / panel: set image by digest, wait for the rollout, annotate."""
    namespace = name
    base = ["kubectl", "--context", target.kube_context, "-n", namespace]
    current = ""
    probe = r.run(base + ["get", f"deploy/{name}", "-o", "json"], check=False, capture=True)
    if probe.rc == 0:
        containers = _json(probe).get("spec", {}).get("template", {}).get("spec", {}).get("containers", [])
        current = next((c.get("image", "") for c in containers if c.get("image")), "")

    set_image = r.run(base + ["set", "image", f"deploy/{name}", f"{name}={ref}"], check=False, capture=True)
    if set_image.rc != 0:
        raise DeployError(
            f"{name}: {ref} was pushed but the Deployment was NOT changed (kubectl set image failed);"
            f" still running {current or 'the old ref'}\n{(set_image.err or set_image.out).strip()}",
            code=2,
        )
    timeout = "5m" if name == "login" else "3m"
    rollout = r.run(base + ["rollout", "status", f"deploy/{name}", f"--timeout={timeout}"],
                    check=False, capture=True)
    if rollout.rc != 0:
        print(
            f"{name}: the new pod ({ref}) did not become ready. Way back (while the previous image"
            f" is still in ECR or the node's cache):\n"
            f"  kubectl --context {target.kube_context} -n {namespace} rollout undo deploy/{name}",
            flush=True,
        )
        raise DeployError(f"{name}: rollout of {ref} failed", code=2)
    r.run(base + ["annotate", "--overwrite", "deploy/" + name, f"{images.LABEL}={content}"])
    print(f"{name}: released {ref} (content {content})")


def release_world(r: Runner, target: Target, server, web, *, yes: bool, ask) -> bool:
    server_ref, server_content = server.ref, server.content
    web_ref, web_content = web.ref, web.content
    # If only one image changed, re-send the other at the ref it runs now.
    server_ref = running_ref(r, target, "server") if images.released_content(r, target, "server") == server_content else server_ref
    web_ref = running_ref(r, target, "web") if images.released_content(r, target, "web") == web_content else web_ref

    ns = target.world_namespace
    question = f"release world {ns}: stop the world (players are disconnected), set {server_ref}/{web_ref}, start? [y/N]"
    if not (yes or ask(question)):
        return False

    env = {"CONTENT_SERVER": server_content, "CONTENT_WEB": web_content}
    if target.registry == "k3d":
        # The twin imports local tags, not ECR digests.
        env["WORLD_UPDATE_ALLOW_TAG"] = "1"
    result = r.run(
        ["scripts/world_update.sh", target.kube_context, ns, server_ref, web_ref],
        check=False, capture=False, env=env,
    )
    if result.rc != 0:
        raise DeployError(
            f"world_update.sh failed (exit {result.rc}); the script printed how to go on."
            " Re-run with the same two refs; do not scale the hosts by hand."
        )
    return True


def wait_world_ready(r: Runner, target: Target, timeout: int = 300) -> None:
    """Coordinator, fleet and each host Deployment must report a ready replica."""
    deadline = time.monotonic() + timeout
    while True:
        ready = True
        for name in ("coordinator", "fleet"):
            response = kubectl(r, target, "get", f"deploy/{name}", "-o", "json")
            if int(_json(response).get("status", {}).get("readyReplicas", 0) or 0) < 1:
                ready = False
        hosts = kubectl(r, target, "get", "deploy", "-l", HOST_SELECTOR, "-o", "json")
        for item in _json(hosts).get("items", []):
            if int(item.get("status", {}).get("readyReplicas", 0) or 0) < 1:
                ready = False
        if ready:
            return
        if time.monotonic() >= deadline:
            raise DeployError("world did not become Ready after the forge")
        time.sleep(2)


def register_args(r: Runner, target: Target, world_ns: str, name: str = "Errantry") -> list[str]:
    """The `world add …` arguments shared with the `world-register` recipe."""
    if "'" in name:
        raise DeployError("the world name must not contain a single quote")
    world_id = world_ns.removeprefix("world-")
    secret = kubectl(
        r, target, "get", "secret", "coordinator-identity",
        "-o", "jsonpath={.data.link\\.cert-sha256\\.cert}",
    )
    pin = ""
    if secret.rc == 0 and secret.out.strip():
        try:
            pin = hashlib.sha256(base64.b64decode(secret.out.strip(), validate=True)).hexdigest()
        except (ValueError, base64.binascii.Error):
            pin = ""
    if (
        len(pin) != 64 or any(c not in "0123456789abcdef" for c in pin)
        or pin == hashlib.sha256(b"").hexdigest()
    ):
        raise DeployError("no usable link.cert-sha256.cert in Secret coordinator-identity")
    args = [
        "world", "add", world_id, "--name", name,
        "--link", f"https://coordinator.{world_ns}.svc.cluster.local:6480",
        "--link-cert", pin,
    ]
    ingress = kubectl(r, target, "get", "ingress", "-o", "jsonpath={.items[0].spec.rules[0].host}")
    host = ingress.out.strip() if ingress.rc == 0 else ""
    if host:
        args += ["--map-url", f"https://{host}/map/{world_id}"]
    return args


def login_command(target: Target) -> list[str]:
    if target.name == "orion":
        return ["just", "orion-login"]
    return ["kubectl", "--context", target.kube_context, "-n", "login",
            "exec", "deploy/login", "--", "login"]


def post_release(r: Runner, target: Target, *, yes: bool, ask, forge_preset: str, report: GameReport) -> None:
    ns = target.world_namespace
    pods = kubectl(r, target, "get", "pods", "-l", "app.kubernetes.io/component=coordinator",
                   "-o", "jsonpath={.items[0].metadata.name}")
    pod = pods.out.strip().removeprefix("pod/")
    readyz = ""
    if pod:
        response = kubectl(r, target, "get", "--raw",
                           f"/api/v1/namespaces/{ns}/pods/{pod}:9100/proxy/readyz")
        readyz = response.out.strip()
    if readyz == "forge":
        world_id = ns.removeprefix("world-")
        question = f"forge world {world_id} with preset {forge_preset}? [y/N]"
        if yes or ask(question):
            r.run(["kubectl", "--context", target.kube_context, "-n", ns,
                   "exec", "deploy/toolbox", "-c", "toolbox", "--", "sh", "-c",
                   FORGE_SH, "sh", "--preset", forge_preset])
            report.forged = True
            wait_world_ready(r, target)
        else:
            print("game: world not forged; run `just world-ctl forge --preset <p>` when ready")

    listing = r.run(["kubectl", "--context", target.kube_context, "-n", "login",
                     "exec", "deploy/login", "--", "login", "world", "list"],
                    check=False, capture=True)
    if ns.removeprefix("world-") not in listing.out:
        args = register_args(r, target, ns)
        r.run(login_command(target) + args)
        report.registered = True


def run(
    r: Runner,
    target: Target,
    components: set[str],
    *,
    yes: bool,
    dry_run: bool,
    ask,
    forge_preset: str,
    push_only: bool = False,
) -> GameReport:
    report = GameReport()
    status = r.run(["git", "status", "--porcelain", "--", "crates", "Cargo.lock", "assets"],
                   check=False, capture=True)
    if status.out.strip():
        print("game: WARNING: the working tree is dirty; the released revision will carry -dirty", flush=True)
        if not ask("the working tree is dirty (-dirty revision): release it anyway? [y/N]"):
            for name in sorted(components):
                report.skipped[name] = "dirty tree, declined"
            print(report.summary())
            return report

    image_names: set[str] = set()
    for name in components:
        image_names.update({"server", "web"} if name == "world" else {name})
    images.build(r, image_names)
    pushed = images.push(r, target, image_names)
    if push_only or dry_run:
        report.pushed = sorted(pushed)
        print(report.summary())
        return report

    for name in ("login", "world", "panel"):
        if name not in components:
            continue
        if name == "world":
            server, web = pushed.get("server"), pushed.get("web")
            if server is None or web is None:
                raise DeployError("world needs both server and web images")
            changed = (
                images.released_content(r, target, "server") != server.content
                or images.released_content(r, target, "web") != web.content
            )
        else:
            changed = images.released_content(r, target, name) != pushed[name].content
        if not changed:
            report.unchanged.append(name)
            print(f"{name}: unchanged")
            continue
        if name == "world":
            if release_world(r, target, server, web, yes=yes, ask=ask):
                report.released.append(name)
            else:
                report.skipped[name] = "declined"
        else:
            release_simple(r, target, name, pushed[name].ref, pushed[name].content)
            report.released.append(name)

    if "world" in report.released:
        post_release(r, target, yes=yes, ask=ask, forge_preset=forge_preset, report=report)
    print(report.summary())
    return report


def main(argv: list[str] | None = None) -> None:
    parser = argparse.ArgumentParser(prog="deploy.game")
    sub = parser.add_subparsers(dest="command", required=True)
    reg = sub.add_parser("register-args", help="print the shell-quoted world add arguments")
    reg.add_argument("context")
    reg.add_argument("namespace")
    reg.add_argument("name", nargs="?", default="Errantry")
    args = parser.parse_args(argv)
    if args.command == "register-args":
        target = next((t for t in TARGETS.values() if t.kube_context == args.context), None)
        if target is None:
            target = Target(name="orion", kube_context=args.context,
                            state="s3", registry="ecr", world_ns=args.namespace)
        print(shlex.join(register_args(Runner(quiet=True), target, args.namespace, args.name)))


if __name__ == "__main__":
    main()
