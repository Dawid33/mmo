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
  `values/kube-prometheus-stack.yaml.tftpl`.
