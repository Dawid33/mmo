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
import shutil
import tempfile
from dataclasses import dataclass
from pathlib import Path
from typing import Iterable

from deploy.runner import DeployError, Runner
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
PLAY_REGION_BLOCKS = 256
PLAY_CHUNK_BLOCKS = 16
BUILD_DIAL_ARGS = [
    f"region_blocks={PLAY_REGION_BLOCKS}",
    f"chunk_blocks={PLAY_CHUNK_BLOCKS}",
]
BUILD_DIAL_TEXT = (
    f"region_blocks={PLAY_REGION_BLOCKS}\n"
    f"chunk_blocks={PLAY_CHUNK_BLOCKS}\n"
)
PUSH_SCRIPTS = {
    "login": ("login", "scripts/orion_login_push.sh"),
    "panel": ("panel", "scripts/orion_panel_push.sh"),
    "server": ("world", "scripts/orion_world_push.sh"),
    "web": ("world", "scripts/orion_world_push.sh"),
}


def build_dial_path(name: str) -> Path:
    if name == "login":
        return REPO / "target/release/login.build-dials"
    if name == "panel":
        return REPO / "target/release/panel.build-dials"
    if name in ("server", "web"):
        return REPO / "target/world-images/build-dials"
    raise ValueError(f"unknown image: {name}")


def verify_build_dials(which: set[str]) -> None:
    """Refuse stale/test-sized artifacts before they reach a Docker build."""
    for name in sorted(which):
        path = build_dial_path(name)
        actual = path.read_text() if path.is_file() else ""
        if actual != BUILD_DIAL_TEXT:
            shown = actual.strip().replace("\n", " ") or "missing"
            raise DeployError(
                f"{name}: refusing deploy build with invalid dials ({shown}); expected "
                f"region_blocks={PLAY_REGION_BLOCKS} chunk_blocks={PLAY_CHUNK_BLOCKS}. "
                f"rebuild through `just {' '.join(BUILD_DIAL_ARGS)} {BUILD_RECIPES[name]}`"
            )


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
        pairs = [
            ("login", root / "target/release/login"),
            ("build-dials", build_dial_path(name)),
            ("Dockerfile", root / "deploy/images/login/Dockerfile"),
        ]
    elif name == "panel":
        pairs = [
            ("panel", root / "target/release/panel"),
            ("build-dials", build_dial_path(name)),
            ("Dockerfile", root / "deploy/images/panel/Dockerfile"),
        ]
    elif name == "server":
        pairs = [
            ("server", root / "target/release/server"),
            ("worldctl", root / "target/release/worldctl"),
            ("build-dials", build_dial_path(name)),
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
            ("build-dials", build_dial_path(name)),
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
        names = ", ".join(sorted(name for name, build_recipe in BUILD_RECIPES.items()
                                  if build_recipe == recipe and name in which))
        print(
            f"game: images: building {names} via "
            f"`just {' '.join(BUILD_DIAL_ARGS)} {recipe}`",
            flush=True,
        )
        r.run(["just", *BUILD_DIAL_ARGS, recipe])
        verify_build_dials(set(name for name, build_recipe in BUILD_RECIPES.items()
                               if build_recipe == recipe and name in which))
        print(f"game: images: build complete for {names}", flush=True)


def digest_out_path(target: Target, group: str) -> Path:
    return REPO / "target/deploy" / target.name / f"digests-{group}.env"


def push(r: Runner, target: Target, which: set[str]) -> dict[str, Pushed]:
    if not which:
        return {}
    verify_build_dials(which)
    result: dict[str, Pushed] = {}
    print("game: images: pushing " + ", ".join(sorted(which)), flush=True)
    if target.registry == "k3d":
        refs = {name: f"mmo/{name}:twin" for name in which}
        for name in sorted(which):
            ref = refs[name]
            print(f"game: images: building twin image {name}", flush=True)
            stage = stage_dir(name)
            try:
                r.run([
                    "docker", "build", "-q",
                    "--label", f"mmo.content-sha256={staged_hash(name)}",
                    "-t", ref, str(stage),
                ])
            finally:
                shutil.rmtree(stage, ignore_errors=True)
            print(f"game: images: twin image {name} built", flush=True)
        print("game: images: importing twin images into mmo", flush=True)
        r.run(["scripts/k3d.sh", "image", "import", *[refs[n] for n in sorted(which)], "-c", "mmo"])
        print("game: images: twin import complete", flush=True)
        return {name: Pushed(ref=refs[name], content=staged_hash(name)) for name in which}

    by_group: dict[str, set[str]] = {}
    for name in which:
        by_group.setdefault(PUSH_SCRIPTS[name][0], set()).add(name)
    for group, names in sorted(by_group.items()):
        print(f"game: images: pushing {', '.join(sorted(names))} to ECR", flush=True)
        script = PUSH_SCRIPTS[sorted(names)[0]][1]
        out = digest_out_path(target, group)
        out.parent.mkdir(parents=True, exist_ok=True)
        env = {"DIGEST_OUT": str(out)}
        if group == "world":
            env["PUSH_IMAGES"] = ",".join(sorted(names))
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
        print(f"game: images: push complete for {', '.join(sorted(names))}", flush=True)
    return result


def stage_dir(name: str) -> Path:
    """A temporary directory with exactly the files the bash push script stages."""
    stage = Path(tempfile.mkdtemp(prefix=f"deploy-stage-{name}-"))
    for rel, path in staged_files(name):
        target = stage / rel
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(path, target)
    return stage


def ensure_latest(r: Runner, target: Target, name: str) -> None:
    """During --infra, make sure a fresh root's ECR repository has an image."""
    if target.registry == "k3d":
        # No registry to check: the twin imports local images into its one node.
        # Without this the login/world/panel apply waits on ImagePullBackOff.
        print(f"infra: image: twin {name} is missing from a fresh node; building it", flush=True)
        build(r, {name})
        push(r, target, {name})
        return
    print(f"infra: image: checking ECR for mmo/{name}:latest", flush=True)
    exists = r.run(
        ["aws", "ecr", "describe-images", "--repository-name", f"mmo/{name}",
         "--image-ids", "imageTag=latest"],
        check=False,
        capture=True,
    )
    if exists.rc == 0:
        print(f"infra: image: mmo/{name}:latest already exists", flush=True)
        return
    if "ImageNotFoundException" not in (exists.err + exists.out):
        raise DeployError(
            f"{name}: cannot check ECR for mmo/{name}:latest\n"
            f"{(exists.err or exists.out).strip() or f'exit {exists.rc}'}"
        )
    print(f"infra: image: mmo/{name}:latest is missing; building and pushing it", flush=True)
    build(r, {name})
    push(r, target, {name})


def released_content(r: Runner, target: Target, name: str) -> str | None:
    """The content hash the workload currently advertises, if any."""
    print(f"game: {name}: reading released content hash", flush=True)
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
    verifier = sub.add_parser("verify-dials", help="refuse non-production staged build dials")
    verifier.add_argument("images", nargs="+", choices=sorted(PUSH_SCRIPTS))
    args = parser.parse_args(argv)
    try:
        if args.command == "hash":
            print(content_hash([p for p in args.directory.rglob("*") if p.is_file()]))
        elif args.command == "verify-dials":
            verify_build_dials(set(args.images))
            print(
                "deploy: build dials verified: "
                f"region_blocks={PLAY_REGION_BLOCKS} chunk_blocks={PLAY_CHUNK_BLOCKS}"
            )
    except DeployError as exc:
        parser.exit(2, f"deploy: {exc}\n")


if __name__ == "__main__":
    main()
