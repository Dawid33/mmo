# Observability on the orion k3s server: metrics (Prometheus -> Thanos -> S3),
# logs (Alloy -> Loki -> S3), traces (OTLP -> Alloy -> Tempo -> S3), profiles
# (Pyroscope -> S3), one Grafana in front. Buckets, IAM and secrets: s3.tf.
# Handoff / design: docs/superpowers/plans/2026-11-06-k3s-observability-stack-handoff.md

locals {
  ns = kubernetes_namespace_v1.monitoring.metadata[0].name
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
    grafana_host      = var.grafana_host
    ingress_class     = var.ingress_class
    cluster_issuer    = var.cluster_issuer
  })]

  depends_on = [
    kubernetes_secret_v1.thanos_objstore,
    kubernetes_secret_v1.grafana_admin,
    aws_route53_record.grafana,
  ]
}

resource "helm_release" "thanos" {
  name      = "thanos"
  namespace = local.ns
  chart     = "${path.module}/charts/thanos"

  values = [yamlencode({
    image          = var.thanos_image
    objstoreSecret = kubernetes_secret_v1.thanos_objstore.metadata[0].name
    sidecarService = "kps-thanos-discovery"
    storageClass   = var.storage_class
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
    bucket        = aws_s3_bucket.telemetry["loki"].bucket
    region        = var.aws_region
    retention     = var.loki_retention
    storage_class = var.storage_class
    size          = var.volume_sizes["loki"]
  })]

  depends_on = [
    helm_release.kube_prometheus_stack,
    kubernetes_secret_v1.s3_env,
    aws_iam_user_policy.telemetry,
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
    bucket        = aws_s3_bucket.telemetry["tempo"].bucket
    endpoint      = local.s3_endpoint
    region        = var.aws_region
    retention     = var.tempo_retention
    storage_class = var.storage_class
    size          = var.volume_sizes["tempo"]
  })]

  depends_on = [
    helm_release.kube_prometheus_stack,
    kubernetes_secret_v1.s3_env,
    aws_iam_user_policy.telemetry,
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
    bucket        = aws_s3_bucket.telemetry["pyroscope"].bucket
    endpoint      = local.s3_endpoint
    region        = var.aws_region
    retention     = var.pyroscope_retention
    storage_class = var.storage_class
    size          = var.volume_sizes["pyroscope"]
  })]

  depends_on = [
    helm_release.kube_prometheus_stack,
    kubernetes_secret_v1.s3_env,
    aws_iam_user_policy.telemetry,
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
