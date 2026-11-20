Grafana dashboards shipped as ConfigMaps (`dashboards.tf`, label `grafana_dashboard=1`,
picked up by the kube-prometheus-stack Grafana sidecar in any namespace). One JSON file per dashboard.

- `traefik.json` — Traefik Official Kubernetes Dashboard (grafana.com 17347, revision 7),
  `__inputs` dropped, uid `traefik`, datasource variable defaulting to Thanos. Its data comes from
  the k3s Traefik's `:9100` metrics entrypoint via the `traefik` PodMonitor
  (`values/kube-prometheus-stack.yaml.tftpl`, `prometheus.additionalPodMonitors`).
- `garage-s3.json` — hand-written, uid `garage-s3`, on the Prometheus datasource variable (default Thanos): the
  buckets of Garage, the object store on this server (`deploy/modules/garage`). Variables: tenant, bucket (multi / All).
  Storage row = size, object count, unfinished uploads, expiry and the public-website flag PER BUCKET, from the
  `garage-bucket-exporter` pod (`modules/garage/exporter.tf`: it asks Garage's admin API on every scrape, once a
  minute — Garage's own metrics have nothing per bucket). Requests row = Garage's own metrics (the `garage`
  ServiceMonitor), which count per S3 operation for the WHOLE store, never per bucket. Health row = Garage's
  verdict on itself, blocks it could not read back (one copy of everything: above 0 is possible damage), disk.
  It replaced `aws-s3.json` (CloudWatch) when the buckets moved from AWS to Garage, 2026-11.
- `world.json` — hand-written, uid `world`, Prometheus (default Thanos) and Loki datasource variables, variable `world`
  (namespace, multi / All): one Errantry world on Kubernetes (`deploy/modules/world`). Stats = hosts live, world
  generation, seconds since the last committed cut, open successions; graphs = registered per pod, cut decisions per
  minute by outcome, successions declared / settled, generation, loop age, container restarts; a Loki panel of the
  `[perf] event=` lines. Data: the `mmo_*` metrics the server serves on pod port `health` (`crates/server/src/health.rs`)
  and kube-state-metrics. Cut, succession, host-count and generation series are real only on the coordinator
  (`role="coordinator"`), and only while it runs world cuts (several hosts). Its alerts are the `world` rule group in
  `values/kube-prometheus-stack.yaml.tftpl`. The header link keeps the selected time range and opens the telemetry
  dashboard for the hidden world id derived from the selected `world-*` namespace.
- `telemetry.json` — hand-written, uid `telemetry`, with Tempo, Loki and Pyroscope datasource variables. Start with
  environment, cluster, service (`errantry-login` / `errantry-world`), role, world and pod; all are resource/profile
  labels, so the defaults remain low-cardinality selectors rather than ad-hoc text searches. It shows recent and error
  traces, TraceQL p50/p95/p99 operation durations, focused login/coordinator/fleet/host views, correlated
  `trace_id=<hex>` logs, warnings/errors, a selected-pod CPU flame graph and a world role comparison. Trace rows carry
  links into Explore. The `trace_id` selector is intentionally manual: paste it from Tempo to line up the Loki panel.
  Tempo's operation quantiles require its TraceQL metrics generator to be enabled; an empty result is not a dashboard
  provisioning failure while that backend capability is disabled.

The Tempo datasource retains trace-to-log links (pod and namespace narrow the Loki stream, then the trace ID is
applied) and maps the stable trace resource attributes to matching Pyroscope tags: `service.name` / `service_name`,
`mmo.role` / `role`, `mmo.world_id` / `world_id`, and `k8s.pod.name` / `pod`. Do not add player, account, entity,
region, host-id, raw URL, SQL, or token values to these selectors or profile labels. Do not edit dashboards or
datasources in Grafana's UI: change JSON/provisioning here, plan the `monitoring` root, and apply that change
separately from workload releases.
