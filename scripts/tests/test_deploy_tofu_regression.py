import pytest
import re
from pathlib import Path

from deploy import tofu
from deploy.runner import Completed, DeployError, FakeRunner
from deploy.targets import ORION


# Regression: a fresh pg-management root made --infra stop on 'No state file was found' (new S3 state is empty, not unreadable)
def test_fresh_s3_root_has_no_state_but_other_state_errors_raise():
    root = ORION.roots[5]
    prefix = ("tofu", tofu.chdir(ORION, root), "state", "list")

    fresh = FakeRunner({
        prefix: Completed(1, err="Error: No state file was found\n"),
    })
    assert tofu.has_state(fresh, ORION, root) is False

    unavailable = FakeRunner({
        prefix: Completed(1, err="Error loading state: access denied"),
    })
    with pytest.raises(DeployError, match="access denied"):
        tofu.has_state(unavailable, ORION, root)


# Regression: ordinary --infra after the live migration defaulted both Orion services back to legacy PostgreSQL and tried to recreate them
def test_orion_service_roots_keep_pg_management_absent_defaults():
    repo = Path(__file__).resolve().parents[2]
    for service in ("login", "keycloak"):
        source = (repo / "deploy/envs/orion" / service / "main.tf").read_text()
        assert re.search(
            r'variable "use_management_db".*?default\s*=\s*true',
            source,
            re.DOTALL,
        )
        assert re.search(
            r'variable "own_postgres".*?default\s*=\s*"absent"',
            source,
            re.DOTALL,
        )
