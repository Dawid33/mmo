# Elastic 6 — Two CloudNativePG Clusters (PostgreSQL Consolidation) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** orion runs exactly two kinds of PostgreSQL under CloudNativePG — the **region** instance (R5's `pg`, a database per world, unchanged) and a new **management** instance holding the login service's and Keycloak's databases — and the login and Keycloak StatefulSet databases are gone once each move is verified. Chat (Zulip) stays on its chart's own PostgreSQL (decision 1).

**Architecture:** a new module `deploy/modules/pg-management` (a CNPG `Cluster` named `pg-management` in its own namespace `pg-management`, one database and one login role per service, its own nightly dump to its own Garage bucket and key) with an orion root and a twin root. The `login` and `keycloak` modules gain a `database` input (the management instance's host, name, role and password, read by their roots from the `pg-management` root's state) and an `own_postgres` switch (`running` → `stopped` → `absent`) that keeps the old StatefulSet's volume until the owner says it can go. Each service moves in its own window: stop writers → `pg_dump` → `pg_restore --no-owner --role=<svc>` → apply with `database` set → verify; the runbook is printed by a recipe, never run by one.

**Tech Stack:** OpenTofu (kubernetes, helm, random providers; S3 remote state on orion, local state on the twin), CloudNativePG (`Cluster`, `Database`, managed roles; the operator from `deploy/modules/cnpg`), Garage (S3), the existing `deploy/modules/postgres/pg-dump.sh`, kube-prometheus-stack rules, `just`, the deploy driver `scripts/deploy/` (Python).

**Spec:** `docs/superpowers/specs/2026-11-14-elastic-region-hosts-design.md` §7 item 6 and §10; the handoff `docs/superpowers/plans/2026-11-14-elastic-region-hosts-planning-handoff.md` §E6 ("Delivers", "Scope", "Must settle in the plan").

## Decisions (settled in planning; the plan proceeds on them — "To confirm" lists which need Dawid)

1. **Chat stays on its own instance.** The Zulip chart's PostgreSQL is Zulip's own image (`deploy/modules/zulip/main.tf:3-5`: "deliberately the chart's own (Zulip's image, with its full-text search …)"): Zulip's search configuration needs dictionary files and its own text-search setup that CloudNativePG's `ghcr.io/cloudnative-pg/postgresql` image does not ship. Moving it means building and maintaining a custom CNPG image FROM that image plus Zulip's dictionaries, and pointing a chart that bundles its database at an external one — a project of its own for one service with no backup need the management instance would meet better than Zulip's own `auto_backup`. Said plainly in the module header, `CLAUDE.md` and `TODO.md`: "orion runs two CNPG clusters and Zulip's bundled PostgreSQL".
2. **Names.** Namespace `pg-management`, `Cluster` `pg-management` (Service `pg-management-rw.pg-management.svc.cluster.local:5432`), single instance, `enableSuperuserAccess: false`. Databases and roles keep the services' current names — `login` (owner `login`), `keycloak` (owner `keycloak`) — so a `pg_restore --no-owner --role=<name>` lands owned by the role the service already connects as. Role passwords: `random_password` in the `pg-management` root, written to a `kubernetes.io/basic-auth` Secret `pg-<svc>` in `pg-management` (the CNPG managed role's `passwordSecret`, labelled `cnpg.io/reload: "true"`) and exported as a sensitive output `databases` that the `login` and `keycloak` roots read (`terraform_remote_state`) to write their own namespace's Secret. A password in state is unavoidable with OpenTofu-made passwords (the region instance does the same); none is ever printed.
3. **Never the region instance's anything.** Its own namespace, `Cluster`, storage (`20Gi`, `local-path`), roles, Secrets, CronJob, bucket `management-backups` and Garage tenant `management` (its own key) — never `postgres` / `pg` / `world-backups` / tenant `world`. No module reads across; `deploy/modules/postgres` is not edited.
4. **PostgreSQL versions:** both StatefulSets run `postgres:17-alpine` and the management instance `ghcr.io/cloudnative-pg/postgresql:17` (the region's pin): a logical dump/restore within major 17. (musl vs glibc collations do not matter for a logical restore; both databases use the default collation.)
5. **Connection budget** (`max_connections` set explicitly to `100` on the management `Cluster`): login `LOGIN_POOL_MAX` 8 × 3 pods during a rollout = 24; Keycloak `KC_DB_POOL_MAX_SIZE` = `20` (set now; Quarkus' default is 100) × 2 pods during its rolling update = 40; the dump Job 2; CNPG's instance manager and `superuser_reserved_connections` (3) ≈ 6 — 72 of 100.
6. **The operator:** the `cnpg` module installs the chart cluster-wide (the region `Cluster` lives outside `cnpg-system`), so nothing beyond R5's install is needed; Task 1 Step 1 confirms it.
7. **Rollback per service** = `database = null`, `own_postgres = "running"` and an apply: the service is back on its old StatefulSet with the data **as of the move** (what was written to the management instance since is lost: login's refresh tokens, kicks and registry edits; Keycloak's new accounts, credentials and realm edits). So a rollback is for the verify window (minutes); after it, a reverse dump/restore (the same runbook with the instances swapped) is the way back. Old volumes: `own_postgres = "stopped"` (StatefulSet at 0 replicas, PVC bound and untouched) after the move; `absent` (StatefulSet, Service and Secret removed; the PVC is NOT deleted by OpenTofu — a StatefulSet's claim outlives it) only after Dawid says so; the PVC is deleted by hand (`kubectl delete pvc`) after that. Recommended: at least 14 days and one verified management dump + restore drill before `absent`.
8. **Backups:** the management `CronJob` `pg-dump` (schedule `45 3 * * *`, 30 minutes after the region's; keep 7) dumps `login` and `keycloak` as their own roles with `deploy/modules/postgres/pg-dump.sh` (read by path: one script; its `WORLD` variable is a label, set to the database name). The alert `WorldBackupFailed` is generalised to `PostgresBackupFailed` over namespaces `postgres|pg-management` (same expression, regex on the namespace). The account → character table is one of the two things that cannot be rebuilt: the first management dump is checked by a restore drill before `own_postgres = "absent"`.

## To confirm with Dawid (the plan proceeds on the recommendation)

1. Chat stays on Zulip's bundled PostgreSQL (decision 1). **Recommended: yes.**
2. Old volumes kept at least 14 days and until one verified management dump + drill (decision 7). **Recommended: yes.**
3. The move order login → Keycloak (smallest blast radius first; the handoff's order), each in its own window, and Keycloak's sign-in outage (every Keycloak session ends with its restart: everyone signs in again). **Recommended: yes.**
4. Who runs the orion windows: **Dawid** (CLAUDE.md: ask before anything on orion). Claude builds, rehearses login's move on the twin and prints each runbook.

## Global Constraints

- OpenTofu, never Terraform; `deploy/**/*.tfvars` and `.env` are git-ignored — never commit or print secrets. On orion, always `just orion-plan <root>` before `just orion-apply <root>`; Claude runs neither (owner's), nor anything else on orion.
- Nothing of the management side goes into the region instance, and neither reads the other's database (design §10). `deploy/modules/postgres` and the `postgres` roots are not edited.
- One CNPG `Cluster` per side; never a shared role, Secret, volume, CronJob, bucket or Garage key.
- Keep the justfile in sync (CLAUDE.md): every new or changed command updates its recipe in the same change; runbook recipes PRINT, never run, a restore.
- `scripts/deploy/targets.py` is being changed by another session (uncommitted at planning time): Task 4 edits it only after that work is committed, and stages only its own hunks (memory: commit only your own paths; check `git diff --cached --name-only` before every commit).
- Only regression tests and gates are committed (CLAUDE.md, Testing): this plan's checks are `tofu validate`, `tofu fmt -check`, twin applies and the twin rehearsal — nothing new is committed as a test.
- Verification per task: `tofu -chdir=<root> init -backend=false && tofu -chdir=<root> validate` for every touched root, `tofu fmt -check -recursive deploy`, and the twin steps the task names (`just twin-apply` / `twin-check` are allowed).

## Review Focus

1. **Players already in a world while login moves:** hosts verify join tokens against login's public keys fetched over HTTPS — a world must keep running and its players keep playing while login is stopped; a join or rejoin answers 503 and succeeds after. Task 2's twin rehearsal checks a joined player stays connected through the move and a rejoin succeeds after.
2. **Ownership after the restore:** a `pg_restore` whose objects are owned by anything but the service's role makes the service's own migrations fail at its next start (`permission denied`). Each runbook checks `select tableowner, count(*) … group by 1` shows one line, the service's role, before the service starts.
3. **Writes after the dump:** a login pod or a Keycloak pod still running during the dump writes rows the new instance never gets. Each runbook scales the service to 0 and waits for no pod before `pg_dump`, and the login sweeper's pending kicks (durable in login's store, migration 0003) are in the dump.
4. **Keycloak's sessions and caches:** sessions live in Keycloak's memory, so its restart signs everyone out; realm config, users and credentials are in the database. Task 3's runbook verifies a sign-in of an existing account and the `game` client's token exchange at login (`/v1/login/oidc`) after the move.
5. **The connection budget under a rollout** (decision 5): a login rollout (3 pods) during Keycloak's rolling update must not exhaust `max_connections`; Task 3 sets `KC_DB_POOL_MAX_SIZE` and the runbook's verify step reads `select count(*) from pg_stat_activity` against 100.

---

## File structure

| File | Responsibility | Task |
|---|---|---|
| `deploy/modules/pg-management/{main,backup,variables,outputs,versions}.tf`, `charts/pg-management/{Chart.yaml,templates/cluster.yaml,templates/databases.yaml}` | the management `Cluster`, roles, Secrets, databases, nightly dump | 1 |
| `deploy/envs/orion/pg-management/{main,providers,backend,versions}.tf`, `terraform.tfvars.example` | orion root (reads `garage-buckets`) | 1 |
| `deploy/envs/local/pg-management/main.tf` | twin root (no backup) | 1 |
| `deploy/envs/orion/garage-buckets/main.tf` | tenant `management`, bucket `management-backups`, output `management_backup` | 1 |
| `deploy/modules/monitoring/values/kube-prometheus-stack.yaml.tftpl` | `PostgresBackupFailed` over both namespaces | 1 |
| `deploy/modules/login/{main,postgres,variables}.tf`, `deploy/envs/{orion,local}/login/main.tf` | `database` input, `own_postgres` switch | 2 |
| `deploy/modules/keycloak/{main,postgres,variables}.tf`, `deploy/envs/orion/keycloak/main.tf` | the same for Keycloak; `KC_DB_POOL_MAX_SIZE` | 3 |
| `justfile` | `orion-pg-mgmt-backup-now`, `pg-move <svc>` (prints), `twin-pg-move-login` | 2, 3, 4 |
| `scripts/deploy/targets.py` | root `pg-management` in both targets; `login`/`keycloak` read it | 4 |
| `deploy/modules/zulip/main.tf` (header only), `CLAUDE.md`, `TODO.md` | decision 1 stated; Deployment paragraph and apply order | 4 |

---

### Task 1: The management instance (module, roots, bucket, backup, alert)

**Files:** as the table's Task 1 rows.

**Interfaces:**
- Produces: module `pg-management` inputs `namespace` (default `"pg-management"`), `services` (`list(string)`, default `["login", "keycloak"]`; validation `^[a-z][a-z0-9_]{0,30}$`), `image` (default `"ghcr.io/cloudnative-pg/postgresql:17"`), `storage_class` (`"local-path"`), `storage_size` (`"20Gi"`), `max_connections` (`100`), `resources`, `backup` (`object({endpoint, bucket, access_key, secret_key})`, nullable, sensitive), `backup_keep` (`7`), `backup_schedule` (`"45 3 * * *"`), `backup_image` (`"alpine/k8s:1.37.1"`), `backup_scratch_size` (`"10Gi"`). Output `databases` (sensitive): `map(object({ host = string, port = number, database = string, user = string, password = string }))` keyed by service, `host` = `"pg-management-rw.<namespace>.svc.cluster.local"`.
- Produces: `garage-buckets` root output `management_backup` (sensitive, `{endpoint, bucket = "management-backups", access_key, secret_key}`), shaped as its existing world-backup output.

- [ ] **Step 1: Check decision 6** — `helm -n cnpg-system get values cnpg --all` on the twin (or the `cnpg` module's values) shows the operator is cluster-wide (`config.clusterWide` true / unset default). Stop and report if it is namespace-scoped.
- [ ] **Step 2: Write the module.** The chart mirrors `deploy/modules/postgres/charts/pg-cluster` with `metadata.name: pg-management`, `postgresql.parameters.max_connections`, one managed role per service (`login: true`, no superuser / createdb / createrole, `passwordSecret: pg-<svc>`) and one `Database` per service (`name` and `owner` = the service, `databaseReclaimPolicy: retain`). `backup.tf` mirrors `deploy/modules/postgres/backup.tf` with `for_each = var.services`, `WORLD` = the service name, `PGDATABASE` = the service name, the role's Secret, and the script from `file("${path.module}/../postgres/pg-dump.sh")`. No `max_prepared_transactions` (no two-phase cuts here).
- [ ] **Step 3: The roots and the bucket.** `garage-buckets`: a tenant `management` with bucket `management-backups` and the output above. Orion root `pg-management` (S3 backend key `orion/pg-management.tfstate`, reads `garage-buckets`; header comment with the apply order `platform -> garage-buckets -> pg-management -> keycloak -> login`). Twin root: `backup = null`, `storage_size = "2Gi"`.
- [ ] **Step 4: The alert.** Rename `WorldBackupFailed` → `PostgresBackupFailed`; `namespace=~"postgres|pg-management"` in both selectors; description names `{{ $labels.namespace }}`.
- [ ] **Step 5: Verify.** `tofu fmt -check -recursive deploy`; `tofu validate` of `envs/orion/pg-management`, `envs/local/pg-management`, `envs/orion/garage-buckets`, `envs/orion/monitoring`. Expected: clean. Then on the twin: apply `envs/local/pg-management`; `kubectl --context k3d-mmo -n pg-management wait cluster/pg-management --for=condition=Ready --timeout=300s` and `kubectl … wait database/login database/keycloak --for=jsonpath='{.status.applied}'=true`. Expected: both met. `psql` as `login` (password from Secret `pg-login`) runs `select current_user` → `login`.
- [ ] **Step 6: Commit** `feat(deploy): the management CloudNativePG instance (login and Keycloak databases, its own dump and bucket)`.

### Task 2: The login service moves (module switch, runbook, twin rehearsal)

**Files:** `deploy/modules/login/{main,postgres,variables}.tf`, `deploy/envs/{orion,local}/login/main.tf`, `justfile`.

**Interfaces:**
- Consumes: Task 1's `databases["login"]` output.
- Produces: module `login` inputs `database` (`object({host, port, database, user, password})`, nullable, sensitive, default `null` = today's own StatefulSet) and `own_postgres` (`string`, `"running" | "stopped" | "absent"`, default `"running"`; validation; `database == null` requires `"running"`). Recipe `pg-move ctx ns svc` (prints).

- [ ] **Step 1: The switch.** With `database` set, the login Deployment's `DATABASE_URL` is built from it (password via the namespace Secret `login-db`, now holding the management role's password, as `$(PGPASSWORD)` today: `main.tf:209`) and `depends_on` drops the StatefulSet. `own_postgres`: `running` = as today; `stopped` = the StatefulSet at 0 replicas (`wait_for_rollout = false`), Service and its own password kept in a renamed Secret `login-db-old` so the volume's password survives; `absent` = StatefulSet, Service and `login-db-old` gone (`postgres_image`, `postgres_volume_size`, `memory_limits["postgres"]` stay until Task 4 removes them). Roots: orion reads `orion/pg-management.tfstate`; the twin reads the local state of `envs/local/pg-management`. A root variable `use_management_db` (bool, default `false`) and `own_postgres` pass through, so the cut-over is one tfvars change.
- [ ] **Step 2: The runbook recipe** `pg-move ctx ns svc` PRINTS (runs nothing), for `svc` in `login|keycloak`, from the source StatefulSet `<svc>-postgres` in `ns` to `pg-management`: 1) scale the service's Deployment to 0 and `kubectl wait --for=delete pod -l <selector>`; 2) `pg_dump -Fc -U <svc> -d <svc>` from `<svc>-postgres-0` to a local file; 3) find the management primary (`-l cnpg.io/cluster=pg-management,cnpg.io/instanceRole=primary`); 4) `pg_restore --no-owner --role=<svc> -d <svc>` into it (the database exists empty, from Task 1); 5) the owners check (one line `<svc>|<n>`) and a row count of one table per service (`login`: `select count(*) from accounts`; `keycloak`: `select count(*) from user_entity`) on both sides; 6) set `use_management_db = true`, `own_postgres = "stopped"` and plan + apply the service's root; 7) verify (per service, below) and `select count(*) from pg_stat_activity` < 100; 8) the rollback (decision 7). Check the table names against the services' schemas when writing it.
- [ ] **Step 3: Rehearse on the twin** (`just twin-pg-move-login`, a recipe that RUNS the printed steps against `k3d-mmo` / `login`, the twin only): before, `just twin-check` phases 1–3 PASS and a player joined (phase 13's join) stays connected; run the move; after, a rejoin answers 200 and `login account issue-refresh <email>` works. Expected: the twin's account count equal before and after; the world's host logs no auth error during the move.
- [ ] **Step 4: Verify.** `tofu validate` both login roots; `just --list` shows both recipes. Expected: clean.
- [ ] **Step 5: Commit** `feat(deploy): login reads the management instance; the move is printed and rehearsed on the twin`.

### Task 3: Keycloak moves (module switch, pool size, runbook)

**Files:** `deploy/modules/keycloak/{main,postgres,variables}.tf`, `deploy/envs/orion/keycloak/main.tf`.

**Interfaces:**
- Consumes: Task 1's `databases["keycloak"]`; Task 2's `pg-move` recipe (it already prints `keycloak`).
- Produces: module `keycloak` inputs `database` and `own_postgres` exactly as Task 2's; `KC_DB_URL` = `jdbc:postgresql://<host>:<port>/<database>` when set.

- [ ] **Step 1: The switch**, as Task 2 Step 1 (Secret `keycloak-db` / `keycloak-db-old`; `depends_on` drops the StatefulSet when moved). Add `KC_DB_POOL_MAX_SIZE = "20"` and `KC_DB_POOL_MIN_SIZE = "2"` to the env in both modes (decision 5).
- [ ] **Step 2: The verify list** in `pg-move`'s `keycloak` branch: the admin console at `https://keycloak.vpn.errantry.renownandruin.com/admin/` loads the realm; an existing account signs in through the `orion` theme; `POST /v1/login/oidc` at login exchanges its token; `just site-theme-check` is unaffected (no image change).
- [ ] **Step 3: Verify.** `tofu validate` of `envs/orion/keycloak`; `tofu fmt -check`. No twin run (the twin has no Keycloak). Expected: clean.
- [ ] **Step 4: Commit** `feat(deploy): Keycloak reads the management instance; its pool bounded`.

### Task 4: The driver, the cleanup switch and the docs

**Files:** `scripts/deploy/targets.py`, `justfile`, `deploy/modules/{login,keycloak}/{postgres,variables}.tf`, `deploy/modules/zulip/main.tf` (header), `CLAUDE.md`, `TODO.md`.

**Interfaces:**
- Consumes: Tasks 1–3.
- Produces: `Root("pg-management", reads=("garage-buckets",), wait=True)` before `keycloak` on orion (and before `login` on the twin); `keycloak` and `login` read `pg-management`. Recipe `orion-pg-mgmt-backup-now` (as `orion-pg-backup-now`, namespace `pg-management`).

- [ ] **Step 1: The driver** (only after the other session's `scripts/deploy` work is committed; its own hunks only). `wait_databases` gains the management `Database`s (`login`, `keycloak`) in namespace `pg-management` for that root. Run the deploy tests that exist (`python3 -m pytest scripts/tests -q`). Expected: pass.
- [ ] **Step 2: `absent` cleans up**: with `own_postgres = "absent"` the per-service variables `postgres_image`, `postgres_volume_size` and `memory_limits["postgres"]` are unused — remove them and the StatefulSet code paths once Dawid confirms both moves (this step waits for decision 2's window; until then it is listed in `TODO.md` as owed).
- [ ] **Step 3: Docs.** Zulip module header: decision 1. `CLAUDE.md` Deployment: the apply order `registry → platform → garage-buckets (+ management-backups) → postgres → pg-management → dns → keycloak → login → …`, "two CloudNativePG clusters — `pg` (region: a database per world, namespace `postgres`) and `pg-management` (login, Keycloak; namespace `pg-management`); Zulip keeps its bundled PostgreSQL", the recipes, the alert's new name. `TODO.md`: an Elastic 6 block with the two orion windows (Dawid's), the volumes' keep-until and the owed cleanup.
- [ ] **Step 4: Verify.** `tofu fmt -check -recursive deploy`; `just --list` parses; `python3 scripts/test_inventory.py check` shows nothing of this plan's. Expected: clean.
- [ ] **Step 5: Commit** `docs(elastic 6): two CloudNativePG clusters — the driver, the recipes, CLAUDE.md, TODO.md`.

## Not in this plan

- Moving chat's database (decision 1), a custom CNPG image, Zulip's backups.
- High availability (more instances) or point-in-time recovery for either cluster; off-box backups (both buckets are on orion's disk, as today).
- A second cluster for the region side (design §10, "far off").

## Self-review notes

- Handoff "Delivers"/"Scope": the second `Cluster` and its own everything (Task 1, decisions 2–3); per-service windows smallest first (Tasks 2, 3; To confirm 3); removal of the per-service PostgreSQL (`own_postgres = absent`, Task 4 Step 2); connection budget (decision 5, Review Focus 5); backups (decision 8); docs (Task 4). "Must settle": chat (decision 1), versions (decision 4), rollback and volume keep (decision 7), operator (decision 6).
- Names used across tasks: module `pg-management`, output `databases`, inputs `database` / `own_postgres`, root variable `use_management_db`, recipes `pg-move`, `twin-pg-move-login`, `orion-pg-mgmt-backup-now`, alert `PostgresBackupFailed`, bucket `management-backups`, tenant `management`.

## As built (2026-11-20)

Tasks 1–4 are implemented without a live Orion apply or twin mutation. The
`pg-management` CNPG module and Orion/twin roots create login and Keycloak roles/databases,
the private management Garage backup, and the generalized backup alert. Both service
modules can switch from their retained StatefulSet to the management database; `stopped`
preserves each old volume and `*-db-old` password, and the old code paths remain present.
The deploy graph orders `pg-management` before the service roots and waits for both
`Database` CRs.

`pg-move` prints the complete service-specific runbook; `twin-pg-move-login` is restricted
to `k3d-mmo` and performs the rehearsal with account-count/owner, connection-budget, world
host continuity and post-move join checks. No production move, `absent` apply, volume
deletion or old-resource-variable removal was performed. Those remain operator-owned and
are owed only after login and Keycloak have been moved, 14 days and one verified
management backup restore drill have passed, and Dawid confirms cleanup.

Production update (2026-11-20): Dawid explicitly waived the retention window and directed
cleanup. Both Orion services now default to `use_management_db=true` and
`own_postgres="absent"`; the legacy StatefulSets, Services, old Secrets and the two named
PVCs were removed. A normal `just deploy --infra` now preserves pg-management and cannot
recreate the retired databases from root defaults. The management-backup restore drill
remains an operations follow-up.
