# Per-bucket metrics (size, object count, unfinished uploads) for the
# `garage-s3` Grafana dashboard. Garage's own /metrics has nothing per bucket,
# only its admin API does, so this small pod asks the admin API on every scrape
# and answers in Prometheus form (files/bucket_exporter.py: standard library
# only, on a stock python image — no image of our own to build).
#
# It holds the admin token (the whole admin API, not just reads): the provider
# cannot make a narrower one. Same namespace and Secret as Garage itself.

locals {
  exporter_labels = { "app.kubernetes.io/name" = "garage-bucket-exporter" }
  exporter_script = file("${path.module}/files/bucket_exporter.py")
  exporter_port   = 9100
}

resource "kubernetes_config_map_v1" "bucket_exporter" {
  count = var.metrics ? 1 : 0
  metadata {
    name      = "garage-bucket-exporter"
    namespace = local.ns
  }
  data = {
    "bucket_exporter.py" = local.exporter_script
  }
}

resource "kubernetes_deployment_v1" "bucket_exporter" {
  count = var.metrics ? 1 : 0
  metadata {
    name      = "garage-bucket-exporter"
    namespace = local.ns
    labels    = local.exporter_labels
  }
  spec {
    replicas = 1
    selector {
      match_labels = local.exporter_labels
    }
    template {
      metadata {
        labels = local.exporter_labels
        annotations = {
          # The script and the token are read once, at start.
          "checksum/script" = sha256(local.exporter_script)
          "checksum/secret" = sha256(random_password.admin_token.result)
        }
      }
      spec {
        automount_service_account_token = false
        enable_service_links            = false
        security_context {
          run_as_user     = 65534
          run_as_group    = 65534
          run_as_non_root = true
        }
        container {
          name    = "exporter"
          image   = var.bucket_exporter_image
          command = ["python", "/app/bucket_exporter.py"]
          env {
            name  = "GARAGE_ADMIN_URL"
            value = "http://${kubernetes_service_v1.garage.metadata[0].name}.${local.ns}.svc:${local.ports.admin}"
          }
          env {
            name = "GARAGE_ADMIN_TOKEN"
            value_from {
              secret_key_ref {
                name = kubernetes_secret_v1.garage.metadata[0].name
                key  = "GARAGE_ADMIN_TOKEN"
              }
            }
          }
          env {
            name  = "LISTEN_PORT"
            value = tostring(local.exporter_port)
          }
          env {
            name  = "PYTHONUNBUFFERED"
            value = "1"
          }
          port {
            name           = "metrics"
            container_port = local.exporter_port
          }
          volume_mount {
            name       = "script"
            mount_path = "/app"
            read_only  = true
          }
          security_context {
            allow_privilege_escalation = false
            read_only_root_filesystem  = true
            capabilities {
              drop = ["ALL"]
            }
          }
          resources {
            requests = { cpu = "5m", memory = "32Mi" }
            limits   = { memory = "128Mi" }
          }
          readiness_probe {
            http_get {
              path = "/healthz"
              port = "metrics"
            }
            period_seconds = 10
          }
          liveness_probe {
            http_get {
              path = "/healthz"
              port = "metrics"
            }
            initial_delay_seconds = 10
            period_seconds        = 30
          }
        }
        volume {
          name = "script"
          config_map {
            name = kubernetes_config_map_v1.bucket_exporter[0].metadata[0].name
          }
        }
      }
    }
  }
}

resource "kubernetes_service_v1" "bucket_exporter" {
  count = var.metrics ? 1 : 0
  metadata {
    name      = "garage-bucket-exporter"
    namespace = local.ns
    labels    = local.exporter_labels
  }
  spec {
    selector = local.exporter_labels
    port {
      name        = "metrics"
      port        = local.exporter_port
      target_port = "metrics"
    }
  }
}

# Bucket sizes move slowly, and every scrape is one admin API call per bucket.
resource "kubernetes_manifest" "bucket_exporter_monitor" {
  count = var.metrics ? 1 : 0
  manifest = {
    apiVersion = "monitoring.coreos.com/v1"
    kind       = "ServiceMonitor"
    metadata = {
      name      = "garage-bucket-exporter"
      namespace = local.ns
      labels    = local.exporter_labels
    }
    spec = {
      selector = { matchLabels = local.exporter_labels }
      endpoints = [{
        port          = "metrics"
        path          = "/metrics"
        interval      = "60s"
        scrapeTimeout = "30s"
      }]
    }
  }
  depends_on = [kubernetes_service_v1.bucket_exporter]
}
