# Observability on the orion k3s server: metrics (Prometheus -> Thanos -> S3),
# logs (Alloy -> Loki -> S3), traces (OTLP -> Alloy -> Tempo -> S3), profiles
# (Pyroscope -> S3), one Grafana in front. S3 is Garage on this server: s3.tf.
# Handoff / design: docs/superpowers/plans/2026-11-06-k3s-observability-stack-handoff.md

locals {
  ns = kubernetes_namespace_v1.monitoring.metadata[0].name

  # Public: TLS at Traefik. Not public: the operator VPN's tunnel is the encryption.
  grafana_url = "${var.grafana_public ? "https" : "http"}://${var.grafana_host}"

  # Thanos (sidecar, store, compactor) reads its objstore Secret once, at start.
  # This goes on those pods as an annotation, so pointing the Secret somewhere
  # else (another endpoint, bucket or key) restarts them. The key's id only,
  # never its secret.
  objstore_checksum = nonsensitive(sha256(join("/", [
    var.object_store.endpoint, local.buckets["thanos"], var.object_store_key.access_key,
  ])))
}

resource "helm_release" "kube_prometheus_stack" {
  name       = "kube-prometheus-stack"
  namespace  = local.ns
  repository = "https://prometheus-community.github.io/helm-charts"
  chart      = "kube-prometheus-stack"
  version    = var.chart_versions["kube_prometheus_stack"]
  timeout    = 900

  values = [templatefile("${path.module}/values/kube-prometheus-stack.yaml.tftpl", {
    namespace         = local.ns
    cluster           = var.cluster_name
    storage_class     = var.storage_class
    retention         = var.prometheus_retention
    retention_size    = var.prometheus_retention_size
    prometheus_size   = var.volume_sizes["prometheus"]
    alertmanager_size = var.volume_sizes["alertmanager"]
    grafana_size      = var.volume_sizes["grafana"]
    thanos_image      = var.thanos_image
    objstore_checksum = local.objstore_checksum
    grafana_host      = var.grafana_host
    grafana_url       = local.grafana_url
    grafana_public    = var.grafana_public
    ingress_class     = var.ingress_class
    cluster_issuer    = var.cluster_issuer

    cpu_alert_threshold_percent = var.cpu_alert_threshold_percent
    cpu_alert_for               = var.cpu_alert_for

    # Only "is it set" reaches the values; the URL itself stays in the Secret.
    zulip_alerts        = nonsensitive(var.zulip_alert_webhook_url != null)
    zulip_alerts_secret = "alertmanager-zulip"

    grafana_oidc           = var.grafana_oidc != null
    grafana_password_login = var.grafana_password_login
    grafana_oidc_secret    = "grafana-oidc"
    grafana_oidc_name      = try(var.grafana_oidc.display_name, "")
    grafana_oidc_client_id = try(var.grafana_oidc.client_id, "")
    grafana_oidc_issuer    = try(var.grafana_oidc.issuer, "")
  })]

  depends_on = [
    kubernetes_secret_v1.grafana_oidc,
    kubernetes_secret_v1.alertmanager_zulip,
    kubernetes_secret_v1.thanos_objstore,
    kubernetes_secret_v1.grafana_admin,
    aws_route53_record.grafana,
  ]
}

# Grafana's OAuth client secret at the identity provider (var.grafana_oidc),
# read by the Grafana pod as GF_AUTH_GENERIC_OAUTH_CLIENT_SECRET.
resource "kubernetes_secret_v1" "grafana_oidc" {
  count = var.grafana_oidc != null ? 1 : 0
  metadata {
    name      = "grafana-oidc"
    namespace = local.ns
  }
  data = {
    client-secret = var.grafana_oidc_client_secret
  }
}

# The Zulip bot URL Alertmanager posts alerts to (it carries the bot's API key).
# Mounted into the Alertmanager pod by the operator (alertmanagerSpec.secrets)
# and read through `url_file`.
resource "kubernetes_secret_v1" "alertmanager_zulip" {
  count = var.zulip_alert_webhook_url != null ? 1 : 0
  metadata {
    name      = "alertmanager-zulip"
    namespace = local.ns
  }
  data = {
    url = var.zulip_alert_webhook_url
  }
}

resource "helm_release" "thanos" {
  name      = "thanos"
  namespace = local.ns
  chart     = "${path.module}/charts/thanos"

  values = [yamlencode({
    image            = var.thanos_image
    objstoreSecret   = kubernetes_secret_v1.thanos_objstore.metadata[0].name
    objstoreChecksum = local.objstore_checksum
    sidecarService   = "kps-thanos-discovery"
    storageClass     = var.storage_class
    retention = {
      raw         = var.thanos_retention_raw
      fiveMinutes = var.thanos_retention_5m
      oneHour     = var.thanos_retention_1h
    }
    store     = { size = var.volume_sizes["thanos_store"] }
    compactor = { size = var.volume_sizes["thanos_compactor"] }
  })]

  # Its ServiceMonitors need the Prometheus Operator CRDs.
  depends_on = [helm_release.kube_prometheus_stack]
}

resource "helm_release" "loki" {
  name       = "loki"
  namespace  = local.ns
  repository = "https://grafana-community.github.io/helm-charts"
  chart      = "loki"
  version    = var.chart_versions["loki"]
  timeout    = 600

  values = [templatefile("${path.module}/values/loki.yaml.tftpl", {
    bucket        = local.buckets["loki"]
    endpoint      = var.object_store.endpoint
    insecure      = var.object_store.insecure
    region        = var.object_store.region
    retention     = var.loki_retention
    storage_class = var.storage_class
    size          = var.volume_sizes["loki"]
  })]

  depends_on = [
    helm_release.kube_prometheus_stack,
    kubernetes_secret_v1.s3_env,
  ]
}

resource "helm_release" "tempo" {
  name       = "tempo"
  namespace  = local.ns
  repository = "https://grafana-community.github.io/helm-charts"
  chart      = "tempo"
  version    = var.chart_versions["tempo"]
  timeout    = 600

  values = [templatefile("${path.module}/values/tempo.yaml.tftpl", {
    bucket        = local.buckets["tempo"]
    endpoint      = var.object_store.endpoint
    insecure      = var.object_store.insecure
    region        = var.object_store.region
    retention     = var.tempo_retention
    namespace     = local.ns
    storage_class = var.storage_class
    size          = var.volume_sizes["tempo"]
  })]

  depends_on = [
    helm_release.kube_prometheus_stack,
    kubernetes_secret_v1.s3_env,
  ]
}

resource "helm_release" "pyroscope" {
  name       = "pyroscope"
  namespace  = local.ns
  repository = "https://grafana.github.io/helm-charts"
  chart      = "pyroscope"
  version    = var.chart_versions["pyroscope"]
  timeout    = 600

  values = [templatefile("${path.module}/values/pyroscope.yaml.tftpl", {
    bucket        = local.buckets["pyroscope"]
    endpoint      = var.object_store.endpoint
    insecure      = var.object_store.insecure
    region        = var.object_store.region
    retention     = var.pyroscope_retention
    storage_class = var.storage_class
    size          = var.volume_sizes["pyroscope"]
  })]

  depends_on = [
    helm_release.kube_prometheus_stack,
    kubernetes_secret_v1.s3_env,
  ]
}

resource "helm_release" "alloy" {
  name       = "alloy"
  namespace  = local.ns
  repository = "https://grafana.github.io/helm-charts"
  chart      = "alloy"
  version    = var.chart_versions["alloy"]

  values = [templatefile("${path.module}/values/alloy.yaml.tftpl", {
    cluster             = var.cluster_name
    loki_push_url       = "http://loki.${local.ns}.svc:3100/loki/api/v1/push"
    tempo_otlp_endpoint = "tempo.${local.ns}.svc:4317"
  })]

  depends_on = [helm_release.loki, helm_release.tempo]
}
