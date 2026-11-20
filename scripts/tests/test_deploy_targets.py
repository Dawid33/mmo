"""The root graph must be complete and topologically ordered (Task 1)."""

from pathlib import Path

from deploy.targets import ORION, TWIN, select

REPO = Path(__file__).resolve().parents[2]


def test_orion_order_respects_reads():
    seen = set()
    for root in ORION.roots:
        for dependency in root.reads:
            assert dependency in seen, f"{root.name} reads {dependency} before it is applied"
        seen.add(root.name)


def test_orion_has_every_root_dir_but_bootstrap():
    dirs = {p.name for p in (REPO / "deploy/envs/orion").iterdir() if p.is_dir()}
    dirs.discard("bootstrap")
    assert {root.name for root in ORION.roots} == dirs


def test_twin_roots_match_local_env_dirs():
    dirs = {p.name for p in (REPO / "deploy/envs/local").iterdir() if p.is_dir()}
    assert [root.name for root in TWIN.roots] == ["platform", "postgres", "login", "world", "panel"]
    assert {root.name for root in TWIN.roots} == dirs


def test_select_only_keeps_order():
    names = [root.name for root in select(ORION, ["panel", "world", "login"])]
    assert names == ["login", "world", "panel"]
    assert select(ORION, []) == list(ORION.roots)
