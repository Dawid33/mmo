"""--game releases, first forge and world registration (Task 5)."""

import base64
import hashlib
import json

import pytest

from deploy import game, images
from deploy.runner import Completed, DeployError, FakeRunner
from deploy.targets import ORION, TWIN

NS = ORION.world_namespace
K = ("kubectl", "--context", "orion", "-n", NS)
CERT = b"CERT"
PIN = hashlib.sha256(CERT).hexdigest()


def deploy_json(image, ready=1, annotations=None):
    doc = {
        "metadata": {"annotations": annotations or {}},
        "spec": {"replicas": 1, "template": {"spec": {"containers": [{"name": "x", "image": image}]}}},
        "status": {"readyReplicas": ready},
    }
    return json.dumps(doc)


def base_responses(*, readyz="ok", login_worlds="ID errantry server", dirty="", world_update_rc=0):
    return {
        ("git", "status", "--porcelain"): Completed(0, dirty),
        ("kubectl", "--context", "orion", "-n", "login", "get", "deploy/login"): Completed(
            0, deploy_json("", annotations={images.LABEL: "old"})
        ),
        ("kubectl", "--context", "orion", "-n", "panel", "get", "deploy/panel"): Completed(
            0, deploy_json("", annotations={images.LABEL: "old"})
        ),
        K + ("get", "deploy/coordinator", "-o"): Completed(0, deploy_json("repo@sha256:old-server")),
        K + ("get", "deploy/fleet", "-o"): Completed(0, deploy_json("repo@sha256:old-server")),
        K + ("get", "deploy", "-l"): Completed(0, json.dumps({"items": [json.loads(deploy_json("repo@sha256:old-server"))]})),
        ("scripts/world_update.sh",): Completed(world_update_rc, err="world_update: failed" if world_update_rc else ""),
        K + ("get", "pods", "-l"): Completed(0, "pod/coordinator-abc\n"),
        K + ("get", "--raw"): Completed(0, readyz),
        K + ("exec", "deploy/toolbox"): Completed(0),
        ("kubectl", "--context", "orion", "-n", "login", "exec", "deploy/login"): Completed(0, login_worlds),
        K + ("get", "secret", "coordinator-identity"): Completed(0, base64.b64encode(CERT).decode()),
        K + ("get", "ingress"): Completed(0, "game.example"),
    }


def stub_images(monkeypatch, released, content):
    monkeypatch.setattr(game.images, "build", lambda r, which: None)
    monkeypatch.setattr(
        game.images, "push",
        lambda r, target, which: {n: images.Pushed(ref=f"repo@sha256:{n}-new", content=content[n]) for n in which},
    )
    monkeypatch.setattr(game.images, "released_content", lambda r, target, name: released.get(name))


def report_of(r, *, components=None, yes=True, ask=None, **kwargs):
    return game.run(
        r, ORION, components or {"login", "world", "panel"}, yes=yes, dry_run=False,
        ask=ask or (lambda q: True), forge_preset="earth-like", **kwargs,
    )


def test_all_unchanged_does_nothing_and_says_so(monkeypatch):
    stub_images(monkeypatch,
                {"login": "L", "panel": "P", "server": "S", "web": "W"},
                {"login": "L", "panel": "P", "server": "S", "web": "W"})
    r = FakeRunner(base_responses())
    report = report_of(r)
    assert report.released == []
    assert report.unchanged == ["login", "world", "panel"]
    assert not any("world_update.sh" in " ".join(c) for c in r.calls)
    assert not any(len(c) > 3 and c[3] == "set" for c in r.calls)


def test_login_released_and_annotated(monkeypatch):
    stub_images(monkeypatch,
                {"panel": "P", "server": "S", "web": "W"},
                {"login": "L-new", "panel": "P", "server": "S", "web": "W"})
    r = FakeRunner(base_responses())
    report = report_of(r)
    assert "login" in report.released
    calls = [" ".join(c) for c in r.calls]
    assert any("set image deploy/login login=repo@sha256:login-new" in c for c in calls)
    assert any("rollout status deploy/login" in c for c in calls)
    assert any("annotate --overwrite deploy/login" in c and f"{images.LABEL}=L-new" in c for c in calls)
    assert "world" not in report.released

def test_world_only_web_changed_resends_running_server_ref(monkeypatch):
    stub_images(monkeypatch,
                {"login": "L", "panel": "P", "server": "S"},
                {"login": "L", "panel": "P", "server": "S", "web": "W-new"})
    r = FakeRunner(base_responses())
    report_of(r)
    call = next(c for c in r.calls if c[0] == "scripts/world_update.sh")
    assert call[3] == "repo@sha256:old-server"  # the running server ref
    assert call[4] == "repo@sha256:web-new"


def test_world_release_needs_confirmation_without_yes(monkeypatch):
    stub_images(monkeypatch,
                {"login": "L", "panel": "P", "server": "S"},
                {"login": "L", "panel": "P", "server": "S", "web": "W-new"})
    r = FakeRunner(base_responses())
    asked = []
    report = report_of(r, yes=False, ask=lambda q: (asked.append(q), False)[1])
    assert any("release world" in q for q in asked)
    assert "world" in report.skipped
    assert not any("world_update.sh" in " ".join(c) for c in r.calls)


def test_world_update_failure_stops_run_nonzero(monkeypatch):
    stub_images(monkeypatch, {}, {"login": "L", "panel": "P", "server": "S", "web": "W-new"})
    r = FakeRunner(base_responses(world_update_rc=1))
    with pytest.raises(DeployError, match="world_update"):
        report_of(r, components={"world"})


def test_forge_asked_when_readyz_forge(monkeypatch):
    stub_images(monkeypatch, {}, {"server": "S-new", "web": "W"})
    r = FakeRunner(base_responses(readyz="forge"))
    asked = []
    report_of(r, components={"world"}, yes=False, ask=lambda q: (asked.append(q), True)[1])
    assert any("forge world" in q and "earth-like" in q for q in asked)
    assert any("worldctl forge" in " ".join(c) for c in r.calls)


def test_register_runs_when_world_missing_from_login(monkeypatch):
    stub_images(monkeypatch, {}, {"server": "S-new", "web": "W"})
    r = FakeRunner(base_responses(login_worlds="ID other-server nothing"))
    report = report_of(r, components={"world"})
    assert report.registered is True
    calls = [" ".join(c) for c in r.calls]
    assert any("world add errantry --name Errantry" in c for c in calls)
    assert any("--link https://coordinator.world-errantry.svc.cluster.local:6480" in c for c in calls)
    assert any(f"--link-cert {PIN}" in c for c in calls)
    assert any("--map-url https://game.example/map/errantry" in c for c in calls)


def test_register_args_match_world_register_recipe_output():
    r = FakeRunner(base_responses())
    assert game.register_args(r, ORION, NS) == [
        "world", "add", "errantry", "--name", "Errantry",
        "--link", "https://coordinator.world-errantry.svc.cluster.local:6480",
        "--link-cert", PIN,
        "--map-url", "https://game.example/map/errantry",
    ]


def test_dirty_tree_asks_even_with_yes(monkeypatch):
    stub_images(monkeypatch, {}, {"login": "L", "panel": "P", "server": "S", "web": "W"})
    r = FakeRunner(base_responses(dirty=" M crates/game/src/lib.rs\n"))
    asked = []
    report = report_of(r, yes=True, ask=lambda q: (asked.append(q), False)[1])
    assert any("dirty" in q for q in asked)
    assert report.released == []


def test_world_update_interrupt_propagates(monkeypatch):
    stub_images(monkeypatch, {}, {"server": "S-new", "web": "W"})

    class Interrupted(FakeRunner):
        def run(self, argv, **kwargs):
            if argv and argv[0] == "scripts/world_update.sh":
                raise KeyboardInterrupt
            return super().run(argv, **kwargs)

    with pytest.raises(KeyboardInterrupt):
        report_of(Interrupted(base_responses()), components={"world"})
