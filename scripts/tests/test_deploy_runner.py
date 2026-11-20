"""Runner and FakeRunner contract (Task 1)."""

from deploy.runner import Completed, DeployError, FakeRunner, Runner


def test_fake_runner_matches_longest_prefix_and_records_calls():
    r = FakeRunner(
        {
            ("tofu",): Completed(0, "generic"),
            ("tofu", "show", "-json"): Completed(0, '{"x": 1}'),
        }
    )
    assert r.run(["tofu", "plan"]).out == "generic"
    assert r.run(["tofu", "show", "-json", "file.tfplan"]).out == '{"x": 1}'
    assert r.calls == [["tofu", "plan"], ["tofu", "show", "-json", "file.tfplan"]]


def test_fake_runner_records_stdin_and_env():
    r = FakeRunner()
    r.run(["docker", "login"], stdin=b"secret", env={"PASSWORD": "secret"})
    assert r.stdins == [b"secret"]
    assert r.envs == [{"PASSWORD": "secret"}]


def test_fake_runner_check_raises():
    r = FakeRunner({("false",): Completed(2, err="boom")})
    try:
        r.run(["false"])
    except DeployError as exc:
        assert "exit 2" in str(exc)
    else:
        raise AssertionError("expected DeployError")


def test_runner_runs_a_command_with_capture():
    result = Runner().run(["python3", "-c", "print('hi')"], capture=True)
    assert result.rc == 0
    assert result.out.strip() == "hi"
