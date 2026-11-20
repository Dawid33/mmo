# Operational dashboards

Grafana dashboards are provisioned from `dashboards/*.json` as ConfigMaps by
`../dashboards.tf` (`grafana_dashboard=1`). `dashboards.tf` discovers JSON by
filename, so adding or removing a dashboard needs no Terraform wiring change.
Do not edit these dashboards in Grafana; change the JSON and plan/apply the
`monitoring` root separately from game or login releases.

## Ownership and hierarchy

The six operational dashboards are:

1. `world.json` — **Errantry Operations Overview** (`uid=world`), the landing
   page. It aggregates world service health and points to the platform/identity
   and storage drilldowns without expanding every pod or route.
2. `world-coordinator.json` — **World Coordinator** (`world-coordinator`).
   Owns replica/ticker convergence, durable transitions, mailbox delivery,
   coordinator HTTP outcomes, cuts, saves and successions. This is the only
   operational dashboard with Loki.
3. `world-fleet-hosts.json` — **World Fleet and Hosts**
   (`world-fleet-hosts`). Owns desired host rows, reconciler objects, dynamic
   host readiness, load, land, coordinator-link and expected drain lifecycle.
4. `identity-edge.json` — **Identity and Edge** (`identity-edge`). Owns
   Kubernetes readiness/restarts and scrape-target state for login, Keycloak,
   world web, site and panel, with bounded Traefik ingress context.
5. `databases-backups.json` — **Databases and Backups**
   (`databases-backups`). Owns CNPG pod/workload, backup Job, PVC and Garage
   backup-bucket state for `postgres` and `pg-management`.
6. `kubernetes-releases.json` — **Kubernetes and Releases**
   (`kubernetes-releases`). Owns workload rollout readiness/drift, restarts,
   waiting/image-pull state, resource use, defined operational Jobs and
   **current** image/build identity.

Ownership means the dashboard containing the diagnosis, not the only place a
metric may be referenced. The overview intentionally repeats a small
aggregate to route the operator to the focused dashboard. No dashboard
duplicates Traefik or Garage's detailed panels:

- `traefik.json` remains the official **Traefik** drilldown (`uid=traefik`,
  grafana.com 17347 revision 7). It uses the k3s Traefik `:9100` PodMonitor.
- `garage-s3.json` remains **Garage S3 buckets** (`garage-s3`). Per-bucket
  metrics come from `garage-bucket-exporter`, while request/error/latency
  metrics are Garage's store-wide series. Identity, edge and database
  dashboards link here rather than copying its rows.

`telemetry.json` is a separate investigation dashboard for Tempo traces,
correlated Loki logs and Pyroscope CPU profiles. It is not part of the six
operational-health dashboards. Tempo 3 no longer supports the
`local-blocks` metrics-generator processor. The TraceQL aggregate panels
therefore remain optional and can be empty; trace search/log correlation is
the supported path.

## Datasources and variables

- Every operational dashboard declares `${DS_PROMETHEUS}` and defaults it to
  Thanos. This preserves one historical Prometheus-compatible query path.
- `${DS_LOKI}` is declared only by the coordinator dashboard because that is
  the one operational view with a bounded diagnostic log panel.
- World dashboards expose `cluster` and a bounded `world-.*` namespace
  selector. Host numeric IDs, boots, super-region IDs, coordinates, user/session
  IDs and revisions are never variables.
- Platform variables are allowlisted namespace/cluster selectors, not
  arbitrary label searches. Per-pod selectors are intentionally avoided by
  default; pod detail belongs to Kubernetes tooling or focused drilldowns.
- All dashboards refresh every 30 seconds and default to the last 6 hours.
  Time options are 1 hour, 6 hours, 24 hours and 7 days.
- Queries aggregate by low-cardinality namespace, role, route, outcome,
  component, workload or Job reason. Raw cumulative counters are shown only as
  rates/increases. No `mmo_*` percentile is claimed because the registry has
  no histogram.

The coordinator dashboard's 15 s mailbox/detach boundary and 30 s alert
boundary, 300 s no-cut threshold, 5% coordinator API 5xx threshold, 600‰ host
tick pressure and 6 s reconciler pass readiness match the alert rules in
`../values/kube-prometheus-stack.yaml.tftpl`. Dashboards provide context and
links; Prometheus alert rules remain authoritative.

## Expected lifecycle

Dynamic host names are never reused. Expected host drains, leave successions,
world stops and cold starts can make a Kubernetes host object exist before a
pod is Ready or remain desired briefly while intentionally draining. Compare
transient demand/object/Ready deltas with the relevant fleet alert's duration;
do not treat every mismatch as a static replica deficit. A stopped world is
silent when its Deployments want zero replicas.

`world.json` retains its existing UID so bookmarks continue to land on the new
overview. Dashboard links preserve the selected time range and variables where
they are shared. The link graph is:
[overview](/d/world/errantry-operations-overview) →
[coordinator](/d/world-coordinator/world-coordinator) /
[fleet and hosts](/d/world-fleet-hosts/world-fleet-and-hosts) /
[identity and edge](/d/identity-edge/identity-and-edge) /
[databases and backups](/d/databases-backups/databases-and-backups) /
[Kubernetes and releases](/d/kubernetes-releases/kubernetes-and-releases).
The overview and the relevant focused dashboards also link to
[Traefik](/d/traefik/traefik), [Garage S3](/d/garage-s3/garage-s3-buckets), and
the alert list.

## Explicit instrumentation gaps

These are shown as text panels and must not be inferred from adjacent metrics
or logs:

- **Elastic 5 moves:** no scrapeable current state, age, duration, outcome or
  evacuation metrics exist. No move panel is shipped.
- **Login end-to-end behavior:** no bounded join/kick/refresh/OIDC outcome or
  duration metrics exist. Traefik and coordinator codes are not proxies for
  login correctness or latency.
- **Keycloak:** the scrape target exists, but its exact Micrometer/Quarkus
  series have not been verified for this deployment. No guessed panels are
  shipped.
- **PostgreSQL/CNPG engine:** no verified exporter backs connection, query,
  transaction/lock, WAL or replication metrics for either database cluster.
  Pod/PVC/Job state is not labelled as engine health.
- **Release history:** KSM and `mmo_build_info` show what is running now and
  rollout drift, not historical deployment events. No release-history time
  series or annotation is claimed.
- **Peer links:** `mmo_world_link_up` is a host-to-coordinator link only. There
  is no stable low-cardinality peer-link metric.

## Telemetry correlation

The existing telemetry dashboard starts from low-cardinality resource labels:
environment, cluster, service, role, world and pod. Its manual trace selector
correlates a Tempo trace ID to Loki; Pyroscope maps stable service/role/world/pod
tags. Do not add player, account, entity, region, host-ID, raw URL, SQL or token
values to those selectors. Tempo/Pyroscope links are not repeated on routine
panels where no functional correlation exists.

Orion production explicitly enables continuous profiling for the login and
world roots. Their release binaries include the `pyroscope` feature and send
100 Hz samples to `http://pyroscope.monitoring.svc:4040`; the world module
propagates the same values to coordinator, fleet and dynamic host pods. The
module defaults remain disabled, while the twin's roots retain their own
override.

The coordinator's Loki panel is restricted to the selected bounded world
namespaces and structured `[coordinator]` diagnostic lines. Logs supplement
metrics for convergence, cut, stall, fence, rollback, rebase and conflict
diagnosis; no log volume is a primary KPI.
