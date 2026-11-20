"""Content hashes, push digests and skip-unchanged support (Task 4)."""

import os
import subprocess
import sys
from pathlib import Path

from deploy import images
from deploy.runner import Completed, FakeRunner
from deploy.targets import ORION, TWIN

SOURCES = {
    "login": {"target/release/login": b"login-bin", "deploy/images/login/Dockerfile": b"login-dockerfile"},
    "panel": {"target/release/panel": b"panel-bin", "deploy/images/panel/Dockerfile": b"panel-dockerfile"},
    "server": {
        "target/release/server": b"server-bin",
        "target/release/worldctl": b"worldctl-bin",
        "deploy/images/server/Dockerfile": b"server-dockerfile",
    },
    "web": {
        "target/web/index.html": b"<html>",
        "deploy/images/web/Dockerfile": b"web-dockerfile",
        "deploy/images/web/nginx.conf": b"nginx-conf",
    },
}


def make_sources(repo: Path, names):
    for name in names:
        for rel, data in SOURCES[name].items():
            path = repo / rel
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)


def write_digests(repo: Path, target, group, entries):
    path = images.digest_out_path(target, group)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("".join(f"{name}={ref}\n" for name, ref in entries.items()))


def test_content_hash_stable_and_order_independent(tmp_path):
    a, b = tmp_path / "a", tmp_path / "b"
    a.write_bytes(b"one")
    b.write_bytes(b"two")
    assert images.content_hash([a, b]) == images.content_hash([b, a])
    assert images.content_hash([a, b]) == images.content_hash([a, b])


def test_content_hash_changes_with_one_byte(tmp_path):
    a, b = tmp_path / "a", tmp_path / "b"
    a.write_bytes(b"one")
    b.write_bytes(b"two")
    before = images.content_hash([a, b])
    b.write_bytes(b"two!")
    assert images.content_hash([a, b]) != before


def test_push_reads_digest_out_file(tmp_path, monkeypatch):
    monkeypatch.setattr(images, "REPO", tmp_path)
    make_sources(tmp_path, ["server", "web"])
    write_digests(tmp_path, ORION, "world", {"server": "repo@sha256:aaa", "web": "repo@sha256:bbb"})
    r = FakeRunner()
    pushed = images.push(r, ORION, {"server", "web"})
    assert pushed["server"].ref == "repo@sha256:aaa"
    assert pushed["web"].ref == "repo@sha256:bbb"
    assert pushed["server"].content == images.staged_hash("server")
    assert any("orion_world_push.sh" in " ".join(call) for call in r.calls)
    assert r.envs[0]["DIGEST_OUT"].endswith("digests-world.env")


def test_login_push_is_push_only(tmp_path, monkeypatch):
    monkeypatch.setattr(images, "REPO", tmp_path)
    make_sources(tmp_path, ["login"])
    write_digests(tmp_path, ORION, "login", {"login": "repo@sha256:ccc"})
    r = FakeRunner()
    pushed = images.push(r, ORION, {"login"})
    assert pushed["login"].ref == "repo@sha256:ccc"
    assert r.envs[0]["PUSH_ONLY"] == "1"


def test_twin_push_imports_images_by_tag(tmp_path, monkeypatch):
    monkeypatch.setattr(images, "REPO", tmp_path)
    make_sources(tmp_path, ["panel"])
    r = FakeRunner()
    pushed = images.push(r, TWIN, {"panel"})
    assert pushed["panel"].ref == "mmo/panel:twin"
    assert any("k3d.sh" in " ".join(call) for call in r.calls)
    assert any("docker" in call for call in r.calls)


def test_ensure_latest_noop_when_present(tmp_path, monkeypatch):
    monkeypatch.setattr(images, "REPO", tmp_path)
    r = FakeRunner({("aws", "ecr", "describe-images"): Completed(0, "{}")})
    images.ensure_latest(r, ORION, "login")
    assert not any("just" in call for call in r.calls)


def test_ensure_latest_pushes_when_absent(tmp_path, monkeypatch):
    monkeypatch.setattr(images, "REPO", tmp_path)
    make_sources(tmp_path, ["login"])
    write_digests(tmp_path, ORION, "login", {"login": "repo@sha256:ddd"})
    r = FakeRunner({("aws", "ecr", "describe-images"): Completed(254, err="ImageNotFoundException")})
    images.ensure_latest(r, ORION, "login")
    assert any("just" in call for call in r.calls)
    assert any("orion_login_push.sh" in " ".join(call) for call in r.calls)


def test_bash_and_python_hash_agree(tmp_path):
    a, b = tmp_path / "a", tmp_path / "b"
    a.write_bytes(b"one")
    b.write_bytes(b"two")
    env = os.environ.copy()
    env["PYTHONPATH"] = str(Path(__file__).resolve().parents[1])
    env.pop("PYTHONHOME", None)
    raw = subprocess.run(
        [sys.executable, "-m", "deploy.images", "hash", str(tmp_path)],
        capture_output=True, text=True, env=env, check=True,
    )
    assert raw.stdout.strip() == images.content_hash([a, b])


def test_released_content_reads_annotation_and_configmap():
    r = FakeRunner(
        {
            ("kubectl", "--context", "orion", "-n", "login", "get", "deploy/login"): Completed(
                0, '{"metadata": {"annotations": {"mmo/content-sha256": "abc"}}}'
            ),
            ("kubectl", "--context", "orion", "-n", "world-errantry", "get", "configmap", "release-history"): Completed(
                0, '{"data": {"released-content-server": "def", "released-content-web": "ghi"}}'
            ),
        }
    )
    assert images.released_content(r, ORION, "login") == "abc"
    assert images.released_content(r, ORION, "server") == "def"
    assert images.released_content(r, ORION, "web") == "ghi"
