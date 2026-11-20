import pytest

from deploy import images
from deploy.runner import DeployError, FakeRunner
from deploy.targets import ORION


# Regression: deploy inherited test region_blocks=64 and restarted Orion's 256-block world into a crash loop (fix forces/verifies 256/16 before push)
def test_production_build_cannot_reuse_test_region_dials(tmp_path, monkeypatch):
    monkeypatch.setattr(images, "REPO", tmp_path)
    marker = images.build_dial_path("server")
    marker.parent.mkdir(parents=True)
    marker.write_text(images.BUILD_DIAL_TEXT)

    r = FakeRunner()
    images.build(r, {"server"})
    assert r.calls == [[
        "just",
        "region_blocks=256",
        "chunk_blocks=16",
        "world-images-build",
    ]]

    marker.write_text("region_blocks=64\nchunk_blocks=16\n")
    push = FakeRunner()
    with pytest.raises(DeployError, match="region_blocks=64"):
        images.push(push, ORION, {"server"})
    assert push.calls == []
