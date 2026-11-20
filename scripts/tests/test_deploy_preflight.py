"""Preflight checks: tools, AWS identity, VPN, tfvars, unplaced roots (Task 3)."""

from pathlib import Path

from deploy import preflight
from deploy.runner import Completed, FakeRunner
from deploy.targets import ORION, Root

GETENT = ("getent", "hosts", "kube-dns.kube-system.svc.cluster.local")


def test_preflight_vpn_only_when_selected_root_needs_it():
    r = FakeRunner({GETENT: Completed(0)})
    preflight.check(r, ORION, [Root("platform")])
    assert not any(c[:1] == ["getent"] for c in r.calls)

    r2 = FakeRunner({GETENT: Completed(2, err="Host not found")})
    problems = preflight.check(r2, ORION, [Root("garage-buckets", needs_vpn=True)])
    assert any("getent" in c for c in r2.calls)
    assert any("VPN down" in p for p in problems)


def test_preflight_aws_identity_failure():
    r = FakeRunner({("aws", "sts", "get-caller-identity"): Completed(1, err="ExpiredToken")})
    problems = preflight.check(r, ORION, [Root("platform")])
    assert any("AWS" in p and "ExpiredToken" in p for p in problems)


def test_preflight_missing_tfvars_named(tmp_path, monkeypatch):
    env = tmp_path / "deploy/envs/orion"
    for root in ORION.roots:
        (env / root.name).mkdir(parents=True)
    monkeypatch.setattr(preflight, "REPO", tmp_path)
    problems = preflight.check(
        FakeRunner(), ORION, [Root("monitoring", needs_tfvars=True), Root("zulip", needs_tfvars=True)]
    )
    assert any("monitoring/terraform.tfvars" in p for p in problems)
    assert any("zulip/terraform.tfvars" in p for p in problems)


def test_preflight_names_an_unplaced_root_dir(tmp_path, monkeypatch):
    env = tmp_path / "deploy/envs/orion"
    for root in ORION.roots:
        (env / root.name).mkdir(parents=True)
    (env / "some-new-root").mkdir()
    monkeypatch.setattr(preflight, "REPO", tmp_path)
    problems = preflight.check(FakeRunner(), ORION, [Root("platform")])
    assert any("some-new-root" in p for p in problems)
