"""Content-hashed images: build, push, digests and released-hash lookups.

A registry digest changes on every build (timestamps), so it cannot say whether
the *content* changed. Every image carries `mmo.content-sha256`, the hash of the
files staged for its docker build (binary/bundle plus Dockerfile, sorted by
relative path). The same hash is recorded on the workload, so --game can skip
an unchanged component.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import tempfile
from dataclasses import dataclass
from pathlib import Path
from typing import Iterable

from deploy.runner import Runner
from deploy.targets import Target

REPO = Path(__file__).resolve().parents[2]
IMAGE_LABEL = "mmo.content-sha256"  # docker build label
LABEL = "mmo/content-sha256"  # kubernetes annotation (slash is legal there)

BUILD_RECIPES = {
    "login": "orion-login-build",
    "panel": "orion-panel-build",
    "server": "world-images-build",
    "web": "world-images-build",
}
PUSH_SCRIPTS = {
    "login": ("login", "scripts/orion_login_push.sh"),
    "panel": ("panel", "scripts/orion_panel_push.sh"),
    "server": ("world", "scripts/orion_world_push.sh"),
    "web": ("world", "scripts/orion_world_push.sh"),
}


@dataclass(frozen=True)
class Pushed:
    ref: str
    content: str


def _hash_entries(entries: Iterable[tuple[str, Path]]) -> str:
    digest = hashlib.sha256()
    for rel, path in sorted(entries, key=lambda item: item[0]):
        data = path.read_bytes()
        digest.update(rel.encode() + b"\0" + str(len(data)).encode() + b"\0" + data)
    return digest.hexdigest()


def content_hash(paths: list[Path]) -> str:
    """Hash files by `relpath\\0len\\0bytes`, relative to their common parent."""
    files = sorted({Path(p).resolve() for p in paths if Path(p).is_file()})
    if not files:
        return hashlib.sha256().hexdigest()
    base = Path(os.path.commonpath([str(path.parent) for path in files]))
    return _hash_entries((path.relative_to(base).as_posix(), path) for path in files)


def staged_files(name: str) -> list[tuple[str, Path]]:
    """The files a push script stages for `name`, with their stage-relative paths."""
    root = REPO
    if name == "login":
        pairs = [("login", root / "target/release/login"), ("Dockerfile", root / "deploy/images/login/Dockerfile")]
    elif name == "panel":
        pairs = [("panel", root / "target/release/panel"), ("Dockerfile", root / "deploy/images/panel/Dockerfile")]
    elif name == "server":
        pairs = [
            ("server", root / "target/release/server"),
            ("worldctl", root / "target/release/worldctl"),
            ("Dockerfile", root / "deploy/images/server/Dockerfile"),
        ]
    elif name == "web":
        web = root / "target/web"
        if not web.is_dir():
            raise FileNotFoundError(f"no {web} (run just world-images-build)")
        pairs = [
            (f"web/{p.relative_to(web).as_posix()}", p)
            for p in web.rglob("*") if p.is_file()
        ]
        pairs += [
            ("Dockerfile", root / "deploy/images/web/Dockerfile"),
            ("nginx.conf", root / "deploy/images/web/nginx.conf"),
        ]
    else:
        raise ValueError(f"unknown image: {name}")
    missing = [str(path) for _, path in pairs if not path.is_file()]
    if missing:
        raise FileNotFoundError(f"missing staged files for {name}: {', '.join(missing)}")
    return sorted(pairs, key=lambda item: item[0])


def staged_hash(name: str) -> str:
    return _hash_entries(staged_files(name))


def build(r: Runner, which: set[str]) -> None:
    """Build through the justfile so the cap and region size stay in one place."""
    for recipe in dict.fromkeys(BUILD_RECIPES[name] for name in sorted(which)):
        r.run(["just", recipe])


def digest_out_path(target: Target, group: str) -> Path:
    return REPO / "target/deploy" / target.name / f"digests-{group}.env"


def push(r: Runner, target: Target, which: set[str]) -> dict[str, Pushed]:
    result: dict[str, Pushed] = {}
    if target.registry == "k3d":
        refs = {name: f"mmo/{name}:twin" for name in which}
        for name in sorted(which):
            ref = refs[name]
            r.run(["docker", "build", "-q", "-t", ref, str(stage_dir(name))])
        r.run(["scripts/k3d.sh", "image", "import", *[refs[n] for n in sorted(which)], "-c", "mmo"])
        return {name: Pushed(ref=refs[name], content=staged_hash(name)) for name in which}

    by_group: dict[str, set[str]] = {}
    for name in which:
        by_group.setdefault(PUSH_SCRIPTS[name][0], set()).add(name)
    for group, names in sorted(by_group.items()):
        script = PUSH_SCRIPTS[sorted(names)[0]][1]
        out = digest_out_path(target, group)
        out.parent.mkdir(parents=True, exist_ok=True)
        env = {"DIGEST_OUT": str(out)}
        if group in ("login", "panel"):
            env["PUSH_ONLY"] = "1"
            env["KUBE_CONTEXT"] = target.kube_context
        r.run([script], env=env)
        lines = out.read_text().splitlines()
        refs = dict(line.split("=", 1) for line in lines if "=" in line)
        for name in names:
            if name not in refs:
                raise RuntimeError(f"{script} wrote no digest for {name} to {out}")
            result[name] = Pushed(ref=refs[name], content=staged_hash(name))
    return result


def stage_dir(name: str) -> Path:
    """A temporary directory with exactly the files the bash push script stages."""
    stage = Path(tempfile.mkdtemp(prefix=f"deploy-stage-{name}-"))
    for rel, path in staged_files(name):
        target = stage / rel
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(path.read_bytes())
    return stage


def ensure_latest(r: Runner, target: Target, name: str) -> None:
    """During --infra, make sure a fresh root's ECR repository has an image."""
    if target.registry != "ecr":
        return
    exists = r.run(
        ["aws", "ecr", "describe-images", "--repository-name", f"mmo/{name}",
         "--image-ids", "imageTag=latest"],
        check=False,
        capture=True,
    )
    if exists.rc == 0:
        return
    build(r, {name})
    push(r, target, {name})


def released_content(r: Runner, target: Target, name: str) -> str | None:
    """The content hash the workload currently advertises, if any."""
    if name in ("login", "panel"):
        namespace, workload = ("login", "login") if name == "login" else ("panel", "panel")
        response = r.run(
            ["kubectl", "--context", target.kube_context, "-n", namespace,
             "get", f"deploy/{workload}", "-o", "json"],
            check=False, capture=True,
        )
        if response.rc != 0:
            return None
        return json.loads(response.out).get("metadata", {}).get("annotations", {}).get(LABEL)
    namespace = target.world_namespace
    key = "released-content-server" if name == "server" else "released-content-web"
    response = r.run(
        ["kubectl", "--context", target.kube_context, "-n", namespace,
         "get", "configmap", "release-history", "-o", "json"],
        check=False, capture=True,
    )
    if response.rc != 0:
        return None
    return json.loads(response.out).get("data", {}).get(key)


def main(argv: list[str] | None = None) -> None:
    parser = argparse.ArgumentParser(prog="deploy.images")
    sub = parser.add_subparsers(dest="command", required=True)
    hasher = sub.add_parser("hash", help="print the content hash of every file in a directory")
    hasher.add_argument("directory", type=Path)
    args = parser.parse_args(argv)
    if args.command == "hash":
        print(content_hash([p for p in args.directory.rglob("*") if p.is_file()]))


if __name__ == "__main__":
    main()
