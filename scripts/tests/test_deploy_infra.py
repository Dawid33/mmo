"""The --infra walk: confirmation, fresh-box rules, guard, waits, hints (Task 3)."""

import json

import pytest

from deploy import infra, tofu
from deploy.runner import Completed, DeployError, FakeRunner
from deploy.targets import ORION, TWIN, Root, select

FIXTURES = __import__("pathlib").Path(__file__).resolve().parent / "fixtures/deploy"
CHANGED = (FIXTURES / "plan_changed.json").read_text()
CREATE_UPDATE = json.dumps(
    {
        "resource_changes": [
            {"address": "kubernetes_namespace.a", "change": {"actions": ["create"]}},
            {"address": "kubernetes_deployment.b", "change": {"actions": ["update"]}},
        ]
    }
)
UNCHANGED = (FIXTURES / "plan_unchanged.json").read_text()


def guard_responses(target, *, replicas="1", image="repo@sha256:abc", marker=False):
    """kubectl fixture: no in-progress marker; one stable server image everywhere."""
    ns = target.world_namespace
    ctx = target.kube_context
    pre = ("kubectl", "--context", ctx, "-n", ns)
    deploys = {
        "items": [
            {
                "metadata": {"name": name},
                "spec": {
                    "replicas": int(replicas) if name == "coordinator" else 1,
                    "template": {"spec": {"containers": [{"name": container, "image": image}]}},
                },
            }
            for name, container in (
                ("coordinator", "coordinator"),
                ("fleet", "fleet"),
                ("toolbox", "toolbox"),
                ("host-h1", "host"),
            )
        ]
    }
    return {
        pre + ("get", "configmap", "release-in-progress"): Completed(0 if marker else 1),
        pre + ("get", "deploy", "-l"): Completed(0, json.dumps(deploys)),
    }


def make_runner(target, *, changed=(), state=(), plan_show=CREATE_UPDATE, replicas="1"):
    responses = guard_responses(target, replicas=replicas)
    for root in target.roots:
        cd = tofu.chdir(target, root)
        responses[("tofu", cd, "state", "list")] = Completed(
            0, "resource.x" if root.name in state else ""
        )
        responses[("tofu", cd, "plan")] = Completed(2 if root.name in changed else 0)
        responses[("tofu", cd, "show", "-json")] = Completed(0, plan_show)
        responses[("tofu", cd, "apply")] = Completed(0)
    return FakeRunner(responses)


def plan_calls(r, target, root):
    return [c for c in r.calls if c[:3] == ["tofu", tofu.chdir(target, root), "plan"]]


def was_applied(r, target, root):
    return any(c[:3] == ["tofu", tofu.chdir(target, root), "apply"] for c in r.calls)


def test_unchanged_root_not_applied_not_asked():
    roots = select(ORION, ["platform", "dns"])
    r = make_runner(ORION)
    asked = []
    report = infra.run(
        r, ORION, roots, yes=False, dry_run=False,
        ask=lambda q: asked.append(q) or False, ensure_image=lambda name: None,
    )
    assert report.unchanged == ["platform", "dns"]
    assert asked == []
    assert not any(was_applied(r, ORION, root) for root in roots)


def test_changed_root_asks_and_applies_on_yes_and_skips_on_no():
    for answer, expect in ((True, ["platform"]), (False, [])):
        roots = select(ORION, ["platform"])
        r = make_runner(ORION, changed={"platform"})
        asked = []
        report = infra.run(
            r, ORION, roots, yes=False, dry_run=False,
            ask=lambda q: asked.append(q) or answer, ensure_image=lambda name: None,
        )
        assert asked == ["apply platform? [y/N]"]
        assert report.applied == expect
        assert was_applied(r, ORION, roots[0]) is bool(expect)
        if not expect:
            assert "declined" in report.skipped["platform"]


def test_yes_flag_auto_applies_create_update():
    roots = select(ORION, ["platform"])
    r = make_runner(ORION, changed={"platform"})
    asked = []
    infra.run(r, ORION, roots, yes=True, dry_run=False,
              ask=lambda q: asked.append(q) or False, ensure_image=lambda name: None)
    assert asked == []
    assert was_applied(r, ORION, roots[0])


def test_yes_flag_still_asks_on_delete_or_replace():
    roots = select(ORION, ["platform"])
    r = make_runner(ORION, changed={"platform"}, plan_show=CHANGED)
    asked = []
    infra.run(r, ORION, roots, yes=True, dry_run=False,
              ask=lambda q: asked.append(q) or True, ensure_image=lambda name: None)
    assert asked == ["apply platform? [y/N]"]


def test_dry_run_applies_nothing():
    roots = select(ORION, ["platform", "dns"])
    r = make_runner(ORION, changed={"platform", "dns"})
    asked = []
    report = infra.run(r, ORION, roots, yes=True, dry_run=True,
                       ask=lambda q: asked.append(q) or True, ensure_image=lambda name: None)
    assert asked == []
    assert report.applied == []
    assert not any(was_applied(r, ORION, root) for root in roots)
    assert all("dry" in reason for reason in report.skipped.values())


def test_garage_metrics_false_then_second_pass():
    roots = select(ORION, ["garage", "monitoring", "picoshare"])
    r = make_runner(ORION, changed={"garage", "monitoring", "picoshare"})
    infra.run(r, ORION, roots, yes=True, dry_run=False,
              ask=lambda q: False, ensure_image=lambda name: None)
    garage_plans = plan_calls(r, ORION, ORION.roots[3])
    assert len(garage_plans) == 2
    assert "-var=metrics=false" in garage_plans[0]
    assert not any(word.startswith("-var=metrics") for word in garage_plans[1])
    # Order: garage(metrics=false) ... monitoring ... garage(no var) ... picoshare.
    names = []
    for call in r.calls:
        for root in ORION.roots:
            if call[:3] == ["tofu", tofu.chdir(ORION, root), "plan"]:
                names.append(root.name)
    assert names == ["garage", "monitoring", "garage", "picoshare"]


def test_garage_single_pass_when_monitoring_has_state():
    roots = select(ORION, ["garage", "monitoring"])
    r = make_runner(ORION, changed={"garage", "monitoring"}, state={"monitoring"})
    infra.run(r, ORION, roots, yes=True, dry_run=False,
              ask=lambda q: False, ensure_image=lambda name: None)
    garage_plans = plan_calls(r, ORION, ORION.roots[3])
    assert len(garage_plans) == 1
    assert not any(word.startswith("-var=metrics") for word in garage_plans[0])


def test_image_ensured_before_first_login_apply():
    events = []

    class LoggingRunner(FakeRunner):
        def run(self, argv, **kwargs):
            events.append(("call",) + tuple(str(a) for a in argv))
            return super().run(argv, **kwargs)

    roots = select(ORION, ["login"])
    r = make_runner(ORION, changed={"login"})
    logged = LoggingRunner(r.responses)
    infra.run(logged, ORION, roots, yes=True, dry_run=False,
              ask=lambda q: False, ensure_image=lambda name: events.append(("ensure", name)))
    assert ("ensure", "login") in events
    ensure_at = events.index(("ensure", "login"))
    plan_at = next(i for i, e in enumerate(events)
                   if e[:4] == ("call", "tofu", tofu.chdir(ORION, ORION.roots[12]), "plan"))
    assert ensure_at < plan_at


def test_world_guard_skips_world_and_reports():
    roots = select(ORION, ["world"])
    r = make_runner(ORION, changed={"world"}, replicas="0")
    report = infra.run(r, ORION, roots, yes=True, dry_run=False,
                       ask=lambda q: False, ensure_image=lambda name: None)
    assert "world" in report.skipped
    assert "stopped" in report.skipped["world"]
    assert plan_calls(r, ORION, ORION.roots[14]) == []


def test_postgres_waits_for_databases():
    roots = select(TWIN, ["postgres"])
    r = make_runner(TWIN, changed={"postgres"})
    infra.run(r, TWIN, roots, yes=True, dry_run=False,
              ask=lambda q: False, ensure_image=lambda name: None)
    assert any("wait" in c and "database/world-twin" in " ".join(c) for c in r.calls)


def test_plan_dial_error_hints_vpn():
    roots = select(ORION, ["garage-buckets"])
    r = make_runner(ORION, changed={"garage-buckets"})
    cd = tofu.chdir(ORION, ORION.roots[4])
    r.responses[("tofu", cd, "plan")] = Completed(1, err="Error: dial tcp 10.99.0.1:443: connect: connection refused")
    with pytest.raises(DeployError, match="VPN down"):
        infra.run(r, ORION, roots, yes=True, dry_run=False,
                  ask=lambda q: False, ensure_image=lambda name: None)
