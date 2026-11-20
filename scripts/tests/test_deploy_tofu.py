"""tofu init / saved plan / show / apply (Task 2)."""

from pathlib import Path

import pytest

from deploy import tofu
from deploy.runner import Completed, DeployError, FakeRunner
from deploy.targets import ORION, TWIN

FIXTURES = Path(__file__).resolve().parent / "fixtures/deploy"
ROOT = ORION.roots[0]
CD = tofu.chdir(ORION, ROOT)


def fixture(name):
    return (FIXTURES / name).read_text()


def runner_for(rc, show_fixture):
    return FakeRunner(
        {
            ("tofu", CD, "plan"): Completed(rc),
            ("tofu", CD, "show", "-json"): Completed(0, show_fixture),
        }
    )


def test_plan_exit0_unchanged():
    r = runner_for(0, fixture("plan_unchanged.json"))
    plan = tofu.plan(r, ORION, ROOT, {})
    assert plan.changed is False
    assert not any((plan.creates, plan.updates, plan.deletes, plan.replaces))


def test_plan_exit2_counts_actions():
    r = runner_for(2, fixture("plan_changed.json"))
    plan = tofu.plan(r, ORION, ROOT, {"metrics": "false"})
    assert plan.changed is True
    assert plan.creates == ["kubernetes_namespace.world"]
    assert plan.updates == ["kubernetes_deployment.login"]
    assert plan.deletes == ["kubernetes_config_map.old"]
    assert plan.replaces == ["kubernetes_service.coordinator"]
    assert any("-var=metrics=false" in call for call in r.calls[0])
    assert "-detailed-exitcode" in r.calls[0]


def test_plan_exit1_raises_with_stderr():
    r = FakeRunner({("tofu", CD, "plan"): Completed(1, err="Error: dial tcp: connect: connection refused")})
    with pytest.raises(DeployError, match="dial tcp"):
        tofu.plan(r, ORION, ROOT, {})


def test_apply_uses_saved_plan_file():
    r = FakeRunner()
    plan = tofu.Plan(path=Path("target/deploy/orion/world.tfplan"), changed=True)
    tofu.apply(r, ORION, ROOT, plan)
    call = r.calls[-1]
    assert call[-1].endswith("world.tfplan")
    assert "-auto-approve" not in call
    assert not any(word.startswith("-var") for word in call)


def test_init_orion_and_twin_use_the_right_state():
    r = FakeRunner()
    tofu.init(r, ORION, ORION.roots[0])
    call = r.calls[-1]
    assert "init" in call
    assert "backend.s3.tfbackend" in " ".join(call)
    assert "orion/platform.tfstate" in " ".join(call)

    tofu.init(r, TWIN, TWIN.roots[0])
    call = r.calls[-1]
    assert "-reconfigure" in call
    twin_env = r.envs[-1] or {}
    assert "target/twin/platform/.terraform" in twin_env.get("TF_DATA_DIR", "")
    assert twin_env.get("TF_PLUGIN_CACHE_DIR", "").endswith("target/tofu-plugin-cache")


def test_has_state_reads_state_list():
    r = FakeRunner({("tofu", CD, "state", "list"): Completed(0, "kubernetes_namespace.world\n")})
    assert tofu.has_state(r, ORION, ROOT) is True
    r2 = FakeRunner({("tofu", CD, "state", "list"): Completed(0, "")})
    assert tofu.has_state(r2, ORION, ROOT) is False
