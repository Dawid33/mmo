# Two-command deploy (`just deploy --infra`, `just deploy --game`) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Deploying orion is two commands. `just deploy --infra` brings every OpenTofu root up to date in the
right order. `just deploy --game` builds, pushes and releases login, panel and the world. Nothing in between is
typed by hand: no `orion-init`/`orion-plan`/`orion-apply` per root, no copied digests, and no forge or
`world-register` steps.

**Architecture:** One stdlib-only Python orchestrator, `scripts/deploy/` (run as `python3 -I -m deploy` with
`PYTHONPATH=scripts`). It holds the root graph as data, drives `tofu`/`kubectl`/`aws`/`docker` through one
`Runner` (which tests replace), and reuses the existing scripts for the delicate parts: `world_update.sh` (the
stop → set → start release) and the three `*_push.sh` scripts (build + push by digest). The per-root recipes
stay as escape hatches, and `deploy` calls the same code paths they do. The twin is a second target of the same
orchestrator (`just target=twin deploy …`), and that is how the whole thing is proven end to end without
touching orion.

**Tech Stack:** Python 3 stdlib (`subprocess`, `json`, `argparse`, `fcntl`), pytest for unit tests, OpenTofu
saved plans plus `tofu show -json`, the existing bash push/update scripts, just 1.42 (`deploy *flags` receives
`--infra` verbatim; checked).

**Spec:** the owner's request (2026-11-15): "the deployment method is super complicated, I want it to be just
two commands, `just deploy --infra` and `just deploy --game` … everything consolidated under two commands."
The current chain this plan replaces is in CLAUDE.md § Deployment and the justfile's orion section.

> **Updated 2026-11-20** for what has landed since: the world no longer runs a `statefulset/hosts` — hosts are
> per-host Deployments (`host-<name>`) made by the `fleet` reconciler from the coordinator's `desired_hosts`
> (Elastic 4, `docs/superpowers/plans/2026-11-19-elastic-4-reconciler-scaling.md`), and the coordinator is an
> HTTPS Service that hosts and login reach without `svc-link` (the coordinator HTTP API,
> `docs/superpowers/plans/2026-11-19-coordinator-http-api.md`). The world's workloads are `coordinator`, `fleet`,
> `toolbox`, `web` plus the host Deployments; `world_update.sh` releases all of them and already writes
> `release-history`. The world-guard and `world-register` specifics below are corrected to match; the forge preset
> default is `earth-like`.

## Global Constraints

- The command names are exactly `just deploy --infra` and `just deploy --game`. Both flags together run infra
  first, then game.
- The old compose game's deploy path is **removed** (owner, 2026-11-15). That means:
  - the recipes `deploy`, `deploy-build`, `deploy-push`, `deploy-logs` and `orion-login-token-push` (it only
    fed the compose host its token);
  - `scripts/deploy_push.sh`;
  - `deploy/web/compose.yml`.

  Kept: `deploy_host` (`tunnel` and `orion-vpn-peer` use it), `web-release` and `scripts/web_release.sh` (the
  world image build uses the script). Stopping the compose containers still running on the box is the owner's
  hand step: say so, never ssh in to do it.
- No secret is ever printed, passed in argv or written outside where it lives now. ECR passwords go through
  `--password-stdin` and tfvars stay git-ignored.
- What gets applied is exactly what was shown. Every root is `tofu plan -out=<file>` followed by
  `tofu apply <file>`, never a bare `apply`.
- Anything that would delete or replace a resource asks for confirmation even with `--yes`.
- A world release is always stop → replace images → start (`scripts/world_update.sh`), never a rolling update
  and never an apply.
- The `world` root is never applied while the world is stopped (its `coordinator` Deployment at 0 replicas) or a
  world update is under way (the `release-in-progress` ConfigMap, or an interrupted update's mixed images across
  the coordinator, fleet, toolbox, web and host Deployments), because that apply starts the coordinator and the
  fleet reconciler on whatever images are set then.
- Every build goes through the justfile's `cap`, i.e. the existing build recipes.
- Keep the justfile in sync: CLAUDE.md § Deployment and the justfile's orion comment block describe the two
  commands first and the per-root recipes as escape hatches.
- No new dependencies: stdlib Python only. The tools used are `tofu`, `kubectl`, `aws`, `docker`, `just`,
  `scripts/k3d.sh`.

## Decisions (made here, change before executing if wrong)

1. **What each command covers.** `--infra` covers every `deploy/envs/orion` root except `bootstrap`, which
   creates the state bucket once and stays manual. The **website** (`site` root, a ConfigMap of `website/site`)
   is in `--infra`. `--game` covers images `mmo/login`, `mmo/panel`, `mmo/server` and `mmo/web`, plus the
   world's first forge and its login registration.
2. **Unchanged components are skipped.** Docker image digests change on every build because of timestamps, so
   digests cannot tell whether anything changed. Each image instead gets a label `mmo.content-sha256`, the
   sha256 over the staged files (binary/bundle plus Dockerfile, in sorted path order). The released hash is
   recorded on the workload: annotation `mmo/content-sha256` on `deploy/login` and `deploy/panel`, and keys
   `released-content-server` / `released-content-web` in the world's `release-history` ConfigMap.
   `--game` skips the push and the release of a component whose hash matches. A world restart that changes
   nothing never happens.
3. **The chicken-and-egg cases are handled inside `--infra`.** Two cases need it:
   - **Images missing on a fresh box:** if a root that needs an image (login, world, panel) has no state yet
     and its ECR repository has no `latest`, `--infra` runs the push-only half of `--game` for that image first.
   - **Garage before monitoring:** `garage` is applied with `-var metrics=false` while `monitoring` has no
     state, then re-planned after `monitoring`.
4. **Confirmation model.** Roots are planned one at a time, in order, because each later root reads earlier
   roots' outputs:
   - A root with no changes prints one line.
   - A root with changes prints its plan summary (create/update/delete/replace counts and addresses) and asks
     `apply <root>? [y/N]`.
   - `--yes` skips the question for create/update-only plans.
   - `--dry-run` plans every root and applies nothing. Later plans may then be stale, and the run says so.
   - `--only <root|login|panel|world>` (repeatable) limits the run.
5. **First-time world.** If after the release the world's `/readyz` answers `forge`, `--game` asks
   `forge world <id> with preset <p>? [y/N]` (`--forge-preset`, default `earth-like` — the first preset,
   `Knobs::default()`) and runs `worldctl forge --preset <p>` in the toolbox (the `world-ctl` shim adds
   `--db "$VOXEL_WORLD_DB"`). If `login world list` has no row for the world, it runs the `login world add …`
   line that `world-register` prints today — the coordinator's HTTPS API
   (`--link https://coordinator.<ns>.svc.cluster.local:6480 --link-cert <pin>`, plus `--map-url` when the world
   has a web Ingress). The arguments are built by one function shared with the `world-register` recipe, which
   now calls it.
6. **Concurrency.** A local `flock` on `target/deploy.lock` prevents two `deploy` runs from this checkout at
   once. Cluster-side, the world guard (above) refuses to apply `world` mid-update.

## Root graph (orion)

Each line gives the order, the root, the roots whose state it reads, and its special handling.

```
 1 platform
 2 dns
 3 registry
 4 garage            metrics=false while monitoring has no state            (fresh-box rule)
 5 garage-buckets    reads garage            needs VPN (garage admin API)
 6 keycloak
 7 vpn-names         after keycloak (gives keycloak.vpn.… its name)
 8 keycloak-realm    reads keycloak          needs VPN + terraform.tfvars
 9 monitoring        reads garage-buckets, keycloak-realm      needs terraform.tfvars
10 garage (again)    only if step 4 ran with metrics=false
11 picoshare
12 zulip             reads garage-buckets, keycloak-realm      needs terraform.tfvars
13 postgres          reads garage-buckets    then wait for every Database CR .status.applied
14 login             reads keycloak-realm, registry            image mmo/login
15 site              reads keycloak-realm
16 world             reads login, postgres, registry           images mmo/server, mmo/web; world guard
17 panel             reads registry          image mmo/panel
```

Twin target: `platform → postgres (wait) → login → world → panel`, local state under `target/twin/<root>/`,
images built as `mmo/<name>:twin` and imported with `scripts/k3d.sh image import`, no ECR, no VPN, no
confirmation (it is a throwaway, so `--yes` is implied as `twin-apply` does today). The twin's world runs the
same Elastic 4 module (`fleet = { min = 2, max = 2 }`): after the forge the coordinator writes two
`desired_hosts` rows and the reconciler makes the two `host-<name>` Deployments — "the world is Ready" means the
coordinator, fleet and both hosts are Ready.

## File structure

- Create `scripts/deploy/__init__.py` (empty) and `scripts/deploy/__main__.py`: argparse CLI, lock, preflight,
  then `infra.run()` and/or `game.run()`.
- Create `scripts/deploy/runner.py`: the `Runner` class (runs commands, captures output and exit codes, prints
  `+ cmd` lines with secrets never in argv) and `FakeRunner` for tests (scripted responses keyed by argv
  prefix, plus a record of the calls).
- Create `scripts/deploy/targets.py`: `Target` dataclass, `ORION` and `TWIN`, and the root graph above as data
  (`Root(name, reads, needs_vpn, needs_tfvars, images, wait, guard, fresh_vars)`).
- Create `scripts/deploy/tofu.py`: init, plan to file, summarize and apply a saved plan. Also
  `has_state(root)` (`tofu state list` non-empty).
- Create `scripts/deploy/infra.py`: walks the graph. Handles the fresh-box rules, the world guard and the
  confirmation.
- Create `scripts/deploy/images.py`: content hash, "is this hash already released", calls the push scripts,
  ECR "has latest" check, twin import.
- Create `scripts/deploy/game.py`: builds, pushes and releases login, panel and world. Also the first forge and
  registration.
- Create `scripts/deploy/preflight.py`: tool presence, AWS identity (`aws sts get-caller-identity`),
  `kubectl --context <ctx> get --raw /livez`, VPN (`getent hosts kube-dns.kube-system.svc.cluster.local`) when
  a selected root needs it, and tfvars presence for selected roots.
- Create `scripts/tests/test_deploy_*.py`: one file per module above, run with
  `python3 -m pytest scripts/tests -q -k deploy`.
- Modify `scripts/orion_world_push.sh`, `scripts/orion_login_push.sh` and `scripts/orion_panel_push.sh`:
  - Add the `mmo.content-sha256` label.
  - Add `DIGEST_OUT=<file>`: write `name=<digest ref>` lines, so nobody parses the stdout.
  - Add `PUSH_ONLY=1`: the login and panel scripts skip `set image`.
- Modify `scripts/world_update.sh`: accept and record `CONTENT_SERVER` / `CONTENT_WEB` into `release-history`.
- Modify `justfile`:
  - Add `deploy *flags` plus a `target` variable.
  - Remove the compose game's deploy recipes and files (Global Constraints).
  - `twin-images` / `twin-apply` become one-line aliases of `just target=twin deploy --game|--infra --only …`.
  - Add a `deploy-test` recipe for the pytest run.
  - Update the orion comment block.
- Modify `CLAUDE.md` § Deployment: lead with the two commands. Point the per-root recipes at "escape hatches";
  shorten, don't add.

---

### Task 1: Runner, targets, root graph

**Files:**
- Create: `scripts/deploy/{__init__,runner,targets}.py`
- Test: `scripts/tests/test_deploy_targets.py`

**Interfaces:**
- Produces:
  - `Runner.run(argv: list[str], *, check=True, capture=False, stdin: bytes|None=None, env: dict|None=None) -> Completed(rc:int, out:str, err:str)`
  - `FakeRunner(responses: dict[tuple[str,...], Completed])`, with `.calls: list[list[str]]`
  - `Root` and `Target(name, kube_context, state: "s3"|"local", roots: list[Root], registry: "ecr"|"k3d")`
  - `ORION`, `TWIN`, `select(target, only: list[str]) -> list[Root]`

- [ ] **Step 1: Write the failing tests.**
  - `test_orion_order_respects_reads`: every root appears after each root in its `reads`.
  - `test_orion_has_every_root_dir_but_bootstrap`: the set of `ORION.roots` names equals the
    `deploy/envs/orion/*` dirs minus `bootstrap`, so a new root dir fails the test until it is placed in the
    graph.
  - `test_twin_roots == ["platform","postgres","login","world","panel"]`, matching
    `deploy/envs/local/*` dirs.
  - `test_select_only_keeps_order`.
- [ ] **Step 2:** Run `python3 -m pytest scripts/tests -q -k deploy_targets`. Expected: FAIL, import error.
- [ ] **Step 3:** Implement as listed. `garage` appears once in `roots`; its second pass is `infra.py`'s job,
  and the root carries `fresh_vars={"metrics": "false", "until_state_of": "monitoring"}`.
- [ ] **Step 4:** Run the same command. Expected: PASS.
- [ ] **Step 5:** Commit `feat(deploy): root graph of the orion and twin targets`.

### Task 2: tofu wrapper — init, saved plan, summary, apply

**Files:**
- Create: `scripts/deploy/tofu.py`
- Test: `scripts/tests/test_deploy_tofu.py`

**Interfaces:**
- Consumes: `Runner`, `Target`, `Root`.
- Produces:
  - `init(r, target, root)`. For orion, the same backend args as `orion-init`. For the twin,
    `TF_DATA_DIR=target/twin/<root>/.terraform` with `-reconfigure`. `TF_PLUGIN_CACHE_DIR=target/tofu-plugin-cache`
    in both cases.
  - `plan(r, target, root, extra_vars: dict) -> Plan(path, changed: bool, creates, updates, deletes, replaces: list[str])`.
    Runs `tofu plan -input=false -detailed-exitcode -out=target/deploy/<target>/<root>.tfplan`
    (exit 0 means unchanged, 2 means changed, 1 raises) and then `tofu show -json <file>` for the
    `resource_changes` actions.
  - `apply(r, target, root, plan)`, which runs `tofu apply -input=false <file>`.
  - `has_state(r, target, root) -> bool`.
- Orion passes `-var kube_context=<ctx>` to every root that declares it. `registry` and `dns` declare it too
  (checked).

- [ ] **Step 1: Write the failing tests** against `FakeRunner` with a recorded `tofu show -json` fixture
  (a small hand-written JSON under `scripts/tests/fixtures/deploy/`):
  - `test_plan_exit0_unchanged`.
  - `test_plan_exit2_counts_actions`. A `["delete","create"]` action counts as a replace.
  - `test_plan_exit1_raises_with_stderr`.
  - `test_apply_uses_saved_plan_file`: argv ends with the `.tfplan` path, with no `-auto-approve` and no `-var`.
- [ ] **Step 2:** Run them. Expected: FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** Run them. Expected: PASS.
- [ ] **Step 5:** Commit `feat(deploy): tofu saved-plan wrapper`.

### Task 3: `--infra` walk — confirmation, fresh-box rules, world guard

**Files:**
- Create: `scripts/deploy/infra.py` and `scripts/deploy/preflight.py`
- Test: `scripts/tests/test_deploy_infra.py`

**Interfaces:**
- Consumes: Tasks 1–2, plus `images.ensure_latest(r, target, image)` from Task 4. In this task, stub it behind a
  parameter `ensure_image: Callable[[str], None]`.
- Produces:
  - `run(r, target, roots, *, yes, dry_run, ask: Callable[[str], bool], ensure_image) -> InfraReport(applied, unchanged, skipped: dict[str,str])`.
  - `preflight.check(r, target, roots) -> list[str]` (problems; empty means go).
  - `world_guard(r, target) -> str|None`, the reason to skip `world`. It returns a reason when:
    - Deployment `coordinator` has `spec.replicas == 0` (the world is stopped);
    - or the running `server` image differs across the coordinator, fleet, toolbox and any host Deployment (an
      interrupted update; `world_update.sh` reads the same images);
    - or a deploy/world-update marker ConfigMap `release-in-progress` exists. `world_update.sh` creates it just
      before its step 2 (the first scale-down) and deletes it after the hosts are Ready; add that in Task 5.

- [ ] **Step 1: Write the failing tests**:
  - `test_unchanged_root_not_applied_not_asked`.
  - `test_changed_root_asks_and_applies_on_yes` / `…_skips_on_no` (a skip is recorded and the walk goes on,
    because later roots plan against the old state, which is correct).
  - `test_yes_flag_auto_applies_create_update`.
  - `test_yes_flag_still_asks_on_delete_or_replace`.
  - `test_dry_run_applies_nothing`.
  - `test_garage_metrics_false_then_second_pass` (no monitoring state). Expected order:
    garage(metrics=false) … monitoring … garage(no var).
  - `test_garage_single_pass_when_monitoring_has_state`.
  - `test_image_ensured_before_first_login_apply` (login has no state, so `ensure_image("login")` is called
    before login's plan).
  - `test_world_guard_skips_world_and_reports` (the coordinator's replicas 0).
  - `test_postgres_waits_for_databases` (the `kubectl wait database/<each>` call after apply).
  - `test_preflight_vpn_only_when_selected_root_needs_it`.
  - `test_preflight_missing_tfvars_named`.
- [ ] **Step 2:** Run them. Expected: FAIL.
- [ ] **Step 3:** Implement the walk. The summary at the end lists applied, unchanged and skipped (with
  reasons), plus the next command: `just deploy --game` when `world` was applied fresh, or
  `just world-start` and then re-run when the world guard fired.
- [ ] **Step 4:** Run them. Expected: PASS.
- [ ] **Step 5:** Commit `feat(deploy): --infra walks every root in order`.

### Task 4: images — content hash, labels, digests, skip-unchanged

**Files:**
- Create: `scripts/deploy/images.py`
- Modify: `scripts/orion_{login,panel,world}_push.sh`
- Test: `scripts/tests/test_deploy_images.py`

**Interfaces:**
- Produces:
  - `content_hash(paths: list[Path]) -> str`: sha256 over `relpath\0len\0bytes` in sorted order.
  - `build(r, which: set[str])`: calls `just world-images-build` / `orion-login-build` / `orion-panel-build`, so
    the cap and region size come from the justfile.
  - `push(r, target, which) -> dict[str, Pushed(ref, content)]`. Orion: the push scripts with
    `PUSH_ONLY=1 DIGEST_OUT=…`, then the file is read back. Twin: `docker build` plus `k3d image import`, with
    ref `mmo/<name>:twin` as today.
  - `ensure_latest(r, target, name)`: for ECR, `aws ecr describe-images --repository-name mmo/<name> --image-ids imageTag=latest`;
    when that is missing, build and push that one image.
  - `released_content(r, target, name) -> str|None`: reads the annotation or the ConfigMap key from decision 2.
- Script changes:
  - All three scripts add `--label mmo.content-sha256=<h>` (computed in bash with the same rule:
    `scripts/deploy` exposes `python3 -I -m deploy.images hash <dir>` so there is one implementation).
  - `DIGEST_OUT` writes `server=…` / `web=…` / `login=…` / `panel=…`.
  - `PUSH_ONLY=1` in login and panel returns right after printing the digest.
  - Without either variable, the scripts behave exactly as before. The recipes `orion-*-push` are unchanged.

- [ ] **Step 1: Write the failing tests**:
  - `test_content_hash_stable_and_order_independent`.
  - `test_content_hash_changes_with_one_byte`.
  - `test_push_reads_digest_out_file`.
  - `test_ensure_latest_noop_when_present` / `…_pushes_when_absent`.
  - `test_bash_and_python_hash_agree`: run the module's `hash` subcommand on a temp dir and compare it with
    `content_hash`.
- [ ] **Step 2:** Run them. Expected: FAIL.
- [ ] **Step 3:** Implement `images.py` and the three script changes.
- [ ] **Step 4:** Run the pytest command. Expected: PASS. Then run `bash -n` on the three scripts.
- [ ] **Step 5:** Commit `feat(deploy): content-hashed images and push-only digests`.

### Task 5: `--game` — release login, panel, world; first forge and registration

**Files:**
- Create: `scripts/deploy/game.py`
- Modify: `scripts/world_update.sh`, and the justfile `world-register` recipe (it now calls the shared function
  and still only prints)
- Test: `scripts/tests/test_deploy_game.py`

**Interfaces:**
- Consumes: Task 4 (`build`, `push`, `released_content`), `world_update.sh`.
- Produces:
  - `run(r, target, components: set[str], *, yes, dry_run, ask, forge_preset) -> GameReport`.
  - `register_args(r, target, world_ns) -> list[str]`: the `world add …` arguments, from the logic moved out of
    the `world-register` recipe — the world id (namespace sans `world-`), `--name`, `--link
    https://coordinator.<ns>.svc.cluster.local:6480`, the `--link-cert` pin (the SHA-256 of Secret
    `coordinator-identity`'s `link.cert-sha256.cert`), and `--map-url` when the world's web Ingress has a host.
- Release rules:
  - **login / panel:** `kubectl set image … <name>=<digest ref>`, then `rollout status`, then
    `annotate --overwrite mmo/content-sha256=<h>`. These are the same steps as the bash scripts, with the
    failure messages carried over: the `rollout undo` line, and exit 2 when `set image` fails. Order: login
    first, so a current login (one whose link protocol matches the world's coordinator) is in place when the
    world's coordinator comes back, then the world, then panel.
  - **world:** asks `release world <ns>: stop the world (players are disconnected), set <server>/<web>, start? [y/N]`.
    `--yes` answers yes for this question, because it is a planned restart, not a destroy. Then it runs
    `CONTENT_SERVER=… CONTENT_WEB=… scripts/world_update.sh <ctx> <ns> <server-ref> <web-ref>` and streams its
    output. A non-zero exit stops the run and repeats the script's own recovery line.
  - Both server and web are released together, as `world_update.sh` requires both. If only one hash changed,
    the other is re-sent at its currently running ref, read from the coordinator/web Deployments (the server ref
    the coordinator, toolbox, fleet and any host Deployment all share), which `world_update.sh` already accepts.
- `world_update.sh` changes (it already stops the fleet reconciler first, then every host Deployment and the
  coordinator, sets the two refs by digest on the coordinator, toolbox, fleet, every host Deployment and web,
  and records the refs it replaces in `release-history` as `previous-*` / `released-*`):
  - Records `released-content-server` / `-web` (the content sha256) in `release-history` beside the refs.
  - Creates ConfigMap `release-in-progress` just before its step 2 (the first scale-down) and deletes it after
    the hosts are Ready; `--infra`'s world guard refuses an apply of `world` while the marker exists.
  - `--rollback` clears the content keys, so the next `--game` releases again (a rollback rewrites the
    `released-*` refs but this script does not re-hash the re-deployed build's content).
- After the release:
  - `/readyz` (through the API server's pod proxy, as `world-status` does) says `forge`: ask, then run
    `worldctl forge --db "$VOXEL_WORLD_DB" --preset <p>` in the toolbox via the same `sh -c` form as `world-ctl`,
    then wait for the coordinator's, the reconciler's and each host Deployment's `status.readyReplicas` (the
    coordinator writes its fleet minimum and the reconciler makes the host Deployments).
  - `login world list` lacks the id: run the `login world add <register_args>` line `world-register` prints (in
    the login pod; on orion that is `just orion-login …`).

- [ ] **Step 1: Write the failing tests**:
  - `test_all_unchanged_does_nothing_and_says_so`.
  - `test_login_released_and_annotated`.
  - `test_world_only_web_changed_resends_running_server_ref`.
  - `test_world_release_needs_confirmation_without_yes`.
  - `test_world_update_failure_stops_run_nonzero`.
  - `test_forge_asked_when_readyz_forge`.
  - `test_register_runs_when_world_missing_from_login`.
  - `test_register_args_match_world_register_recipe_output` (fixture: Secret `coordinator-identity` and a web
    Ingress, matching the current `world-register` output including `--map-url`).
- [ ] **Step 2:** Run them. Expected: FAIL.
- [ ] **Step 3:** Implement `game.py` and the `world_update.sh` changes. Then run
  `scripts/world_update.sh … --dry-run` against the twin to confirm the marker and content keys print.
- [ ] **Step 4:** Run the pytest command. Expected: PASS.
- [ ] **Step 5:** Commit `feat(deploy): --game releases login, world and panel, forges a new world`.

### Task 6: CLI, justfile, twin as a target, docs

**Files:**
- Create: `scripts/deploy/__main__.py`
- Modify: `justfile` and `CLAUDE.md`
- Test: `scripts/tests/test_deploy_cli.py`

**Interfaces:**
- CLI: `python3 -I -m deploy (--infra|--game)… [--target orion|twin] [--yes] [--dry-run] [--only X]… [--forge-preset P]`.
  - It refuses with no mode, or with an unknown `--only` name; the message lists the valid names.
  - Exit codes: 0 done, 1 a step failed, 2 bad arguments or a failed preflight, 3 a guard skipped something
    the run was asked to do (e.g. `world` under `--only world`).
- justfile:

  ```
  target := "orion"
  # Deploy orion (or the twin: just target=twin deploy …). --infra: every OpenTofu root, in order;
  # --game: build, push and release login, world and panel. See scripts/deploy/.
  [positional-arguments]
  deploy *flags:
      flock -n target/deploy.lock env PYTHONPATH=scripts python3 -I -m deploy --target {{target}} "$@"
  ```

  - `deploy-test` runs `python3 -m pytest scripts/tests -q -k deploy`.
  - `twin-images` becomes `just target=twin deploy --game --only login --only panel --only world` with push only
    and no release, which keeps today's behaviour. Add a hidden `--push-only` flag for it.
  - `twin-apply` becomes `just target=twin deploy --infra --yes`.
  - Delete `deploy`, `deploy-build`, `deploy-push`, `deploy-logs` and `orion-login-token-push`, plus
    `scripts/deploy_push.sh` and `deploy/web/compose.yml` (`git rm`). Fix the comment at the `web_server`
    variable that names "`web-release` / `deploy` bundles". CLAUDE.md drops the "OLD compose game on the box,
    `just deploy` …" sentence.

- [ ] **Step 1: Write the failing tests**:
  - `test_no_mode_exits_2`.
  - `test_both_modes_infra_then_game`.
  - `test_unknown_only_lists_names`.
  - `test_guard_skip_exits_3`.
- [ ] **Step 2:** Run them. Expected: FAIL.
- [ ] **Step 3:** Implement. Edit the justfile and the CLAUDE.md § Deployment opening (the two commands and
  what each covers; the old chain becomes "what `deploy` does, and the recipes for doing one step by hand").
- [ ] **Step 4:** Run `just deploy-test`. Expected: all pass.
  - `just --list | grep deploy` should list only `deploy` and `deploy-test`.
  - `grep -rn 'deploy_push\|deploy-build\|orion-login-token-push\|deploy/web/' justfile scripts CLAUDE.md`
    should print nothing.
- [ ] **Step 5:** Commit `feat(just): just deploy --infra / --game; the old compose deploy is gone`.

### Task 7: Prove it on the twin, then a dry run on orion

No new code. This is the acceptance run; fix whatever it finds inside the owning task's files.

- [ ] **Step 1:** Run `just twin-down && just twin-up && just target=twin deploy --infra --game --forge-preset earth-like --yes`.
  Expected: exit 0, five roots applied, the world forged and Ready (the coordinator, fleet and its two host
  Deployments — the twin runs `fleet = { min = 2, max = 2 }`), and `world-register`'s row added. Then
  `just twin-check` passes.
- [ ] **Step 2:** Run `just target=twin deploy --game --yes` again with no source change. Expected:
  "login, panel, world: unchanged", no push, no world restart, and the hosts' pod start times unchanged.
- [ ] **Step 3:** Add a comment line to `crates/panel/src/main.rs`, then run
  `just target=twin deploy --game --yes`. Expected: only panel is rebuilt and released; login and the world
  are unchanged. Revert the comment afterwards.
- [ ] **Step 4:** Stop the twin world (`just world_ctx=k3d-mmo world_ns=world-twin world-stop`: fleet, then hosts,
  then the coordinator to 0), then run `just target=twin deploy --infra --yes`. Expected: `world` is skipped
  with the guard's reason and the exit is 0; with `--only world` the exit is 3.
- [ ] **Step 5:** On orion (VPN up; ask the owner first), run `just deploy --infra --dry-run`. Expected: every
  root plans. With the live box in sync this should be all or almost all "unchanged", and any diff is the
  owner's to read. Do NOT run a real orion `--game` or `--infra` without the owner.
- [ ] **Step 6:** Commit any fixes, plus a short "As built" section appended to this plan.

## Review Focus

- **Expired AWS SSO credentials mid-run.** The `tofu` S3 backend and `aws ecr` fail with an auth error. Expect
  preflight to catch it before the first plan (`aws sts get-caller-identity`), and a mid-run failure to name
  the root it stopped at, with everything before it applied. Test: `test_preflight_aws_identity_failure`
  (Task 3).
- **The VPN dropping between roots** (keycloak-realm or garage-buckets plan fails with a dial error). Expect
  the run to stop at that root with "VPN down? `just orion-vpn-status`", not an opaque tofu trace. Test:
  `test_plan_dial_error_hints_vpn` (Task 3).
- **A dirty working tree.** Expect `--game` to release it but say so loudly (the `-dirty` revision) and
  require confirmation even with `--yes`. Test: `test_dirty_tree_asks_even_with_yes` (Task 5).
- **Ctrl-C during `world_update.sh`.** Expect the orchestrator not to swallow the script's recovery message,
  to forward SIGINT to it, and to exit non-zero. The `release-in-progress` marker stays, so the next
  `--infra` will not apply `world`. Test: `test_world_update_interrupt_propagates` (Task 5).
- **A root added later under `deploy/envs/orion/`.** Expect `--infra` to refuse to run until it is placed in
  the graph, not silently skip it. This is covered by `test_orion_has_every_root_dir_but_bootstrap` (Task 1).
  At runtime, preflight also runs the same check and names the dir.

## Out of scope (stay one-off, by hand)
`bootstrap` (the state bucket), new VPN peers (`orion-vpn-peer`), writing tfvars and Keycloak admin client
secrets, `orion-garage-import`, the Zulip realm link, Postgres restores, and the world rollback
(`orion-world-rollback` stays its own command).

## As built (2026-11-20)

Implemented on `develop` without a worktree. `just deploy --infra` walks the graph from saved plans,
handles the fresh-box image and Garage/monitoring cases, guards a stopped or mixed-image world, waits
for PostgreSQL, and requires explicit confirmation for delete/replace plans. `just deploy --game`
builds through the just recipes, pushes only changed staged content, releases login → world → panel,
records content hashes in workload state, and finishes a first forge plus login registration.

The content hash is the sorted, path-addressed SHA-256 over each staged image. Orion pushes use
`DIGEST_OUT`/`PUSH_ONLY`; world image pushes support a one-image subset so the unchanged server or web
digest is re-used without another push. Twin images are staged with the same content label and imported
through `scripts/k3d.sh`. `world_update.sh` owns the release marker and clears or records the world
content keys alongside its refs.

The previous compose game recipes and files are gone. `world-register` and `deploy --game` share
`scripts/deploy/game.py::register_args`; `twin-images` and `twin-apply` are aliases of the same
orchestrator.

The plan's proposed deploy feature tests were used temporarily to drive and verify the implementation,
then removed: the repository's 2026-11-19 regression-only rule permits no pre-emptive feature tests
without an observed regression or an owner-approved gate. `just deploy-test` is the committed, safe
static check (Python compile, CLI help, and `bash -n` for the release scripts). No real Orion deploy,
twin rebuild, or live-cluster mutation was run during this implementation; the end-to-end twin and Orion
dry-run acceptance steps remain operator-run checks because they need cluster access and, for the twin,
explicit destructive approval.
