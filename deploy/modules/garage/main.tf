# Garage (garagehq.deuxfleurs.fr): the S3-compatible object store of the orion
# k3s server, for the telemetry stack (Thanos, Loki, Tempo, Pyroscope) and
# Zulip's uploads. ONE node, replication factor 1: every object exists once, on
# this box's disk. Nothing here is backed up off the box.
#
# One pod, two PVs (metadata, data) — never more than one replica, and
# `Recreate` so two pods never open the store together. `--single-node` makes
# Garage assign itself the whole cluster layout on first start.
#
#   3900  S3 API     in-cluster http://garage.<ns>.svc:3900, public https://<s3_host>
#   3902  websites   public https://<bucket>.<web_root_domain> (read-only, no key)
#   3903  admin API  in-cluster only: `just orion-garage-admin` forwards it for
#                    the garage-buckets root. Also /health and /metrics.
#
# Buckets, keys and lifecycle rules are NOT made here: modules/garage-buckets.

locals {
  ns     = kubernetes_namespace_v1.garage.metadata[0].name
  labels = { "app.kubernetes.io/name" = "garage" }
  ports  = { s3 = 3900, rpc = 3901, web = 3902, admin = 3903 }

  website_hosts = { for b in var.website_buckets : b => "${b}.${var.web_root_domain}" }
  hosts         = concat([var.s3_host], values(local.website_hosts))

  config = <<-EOT
    metadata_dir = "/var/lib/garage/meta"
    data_dir     = "/var/lib/garage/data"
    db_engine    = "lmdb"

    # One copy of everything: a lost disk is lost data.
    replication_factor = 1
    # No replica to repair the metadata from, so: fsync it, and keep snapshots
    # (<metadata_dir>/snapshots) to fall back on if LMDB is ever corrupted.
    metadata_fsync                  = true
    metadata_auto_snapshot_interval = "6h"

    rpc_bind_addr   = "0.0.0.0:${local.ports.rpc}"
    rpc_public_addr = "127.0.0.1:${local.ports.rpc}"

    [s3_api]
    s3_region     = "${var.s3_region}"
    api_bind_addr = "0.0.0.0:${local.ports.s3}"

    [s3_web]
    bind_addr   = "0.0.0.0:${local.ports.web}"
    root_domain = ".${var.web_root_domain}"
    index       = "index.html"

    [admin]
    api_bind_addr = "0.0.0.0:${local.ports.admin}"
  EOT
}

resource "kubernetes_namespace_v1" "garage" {
  metadata {
    name = var.namespace
  }
}

# Shared secret of the cluster's nodes (one here); Garage wants 32 bytes of hex.
resource "random_id" "rpc_secret" {
  byte_length = 32
}

# Replacing it locks the garage-buckets root out until that root is planned again.
resource "random_password" "admin_token" {
  length  = 48
  special = false
}

resource "random_password" "metrics_token" {
  length  = 48
  special = false
}

# Garage reads these three from the environment (GARAGE_*), so they stay out of
# the config file.
resource "kubernetes_secret_v1" "garage" {
  metadata {
    name      = "garage"
    namespace = local.ns
  }
  data = {
    GARAGE_RPC_SECRET    = random_id.rpc_secret.hex
    GARAGE_ADMIN_TOKEN   = random_password.admin_token.result
    GARAGE_METRICS_TOKEN = random_password.metrics_token.result
  }
}

resource "kubernetes_config_map_v1" "garage" {
  metadata {
    name      = "garage"
    namespace = local.ns
  }
  data = {
    "garage.toml" = local.config
  }
}

resource "kubernetes_persistent_volume_claim_v1" "garage" {
  for_each = var.volume_sizes
  metadata {
    name      = "garage-${each.key}"
    namespace = local.ns
  }
  spec {
    access_modes       = ["ReadWriteOnce"]
    storage_class_name = var.storage_class
    resources {
      requests = { storage = each.value }
    }
  }
  # local-path binds on first consumer (the Deployment below).
  wait_until_bound = false
}

resource "kubernetes_deployment_v1" "garage" {
  metadata {
    name      = "garage"
    namespace = local.ns
    labels    = local.labels
  }
  spec {
    replicas = 1
    strategy {
      type = "Recreate"
    }
    selector {
      match_labels = local.labels
    }
    template {
      metadata {
        labels = local.labels
        annotations = {
          # Config and secrets are read once, at start.
          "checksum/config" = sha256(local.config)
          "checksum/secret" = sha256(jsonencode(kubernetes_secret_v1.garage.data))
        }
      }
      spec {
        # Nothing here talks to the Kubernetes API.
        automount_service_account_token = false
        enable_service_links            = false
        security_context {
          run_as_user     = 1000
          run_as_group    = 1000
          fs_group        = 1000
          run_as_non_root = true
        }
        container {
          name  = "garage"
          image = var.image
          # The image holds the one binary and no shell.
          command = ["/garage", "server", "--single-node"]
          env_from {
            secret_ref {
              name = kubernetes_secret_v1.garage.metadata[0].name
            }
          }
          dynamic "port" {
            for_each = { for k, v in local.ports : k => v if k != "rpc" }
            content {
              name           = port.key
              container_port = port.value
            }
          }
          volume_mount {
            name       = "config"
            mount_path = "/etc/garage.toml"
            sub_path   = "garage.toml"
            read_only  = true
          }
          dynamic "volume_mount" {
            for_each = var.volume_sizes
            content {
              name       = volume_mount.key
              mount_path = "/var/lib/garage/${volume_mount.key}"
            }
          }
          security_context {
            allow_privilege_escalation = false
            read_only_root_filesystem  = true
            capabilities {
              drop = ["ALL"]
            }
          }
          resources {
            requests = { cpu = "50m", memory = "256Mi" }
            limits   = { memory = var.memory_limit }
          }
          # /health needs no token; 200 once the layout is applied and the node is up.
          readiness_probe {
            http_get {
              path = "/health"
              port = "admin"
            }
            period_seconds = 10
          }
          liveness_probe {
            tcp_socket {
              port = "admin"
            }
            initial_delay_seconds = 10
            period_seconds        = 30
          }
        }
        volume {
          name = "config"
          config_map {
            name = kubernetes_config_map_v1.garage.metadata[0].name
          }
        }
        dynamic "volume" {
          for_each = var.volume_sizes
          content {
            name = volume.key
            persistent_volume_claim {
              claim_name = kubernetes_persistent_volume_claim_v1.garage[volume.key].metadata[0].name
            }
          }
        }
      }
    }
  }
}

resource "kubernetes_service_v1" "garage" {
  metadata {
    name      = "garage"
    namespace = local.ns
    labels    = local.labels
  }
  spec {
    selector = local.labels
    dynamic "port" {
      for_each = { for k, v in local.ports : k => v if k != "rpc" }
      content {
        name        = port.key
        port        = port.value
        target_port = port.key
      }
    }
  }
}

# The S3 API host, and one host per website bucket (Garage's web server picks
# the bucket from the Host header). The admin API has no rule: it stays inside.
resource "kubernetes_ingress_v1" "garage" {
  metadata {
    name      = "garage"
    namespace = local.ns
    annotations = {
      "cert-manager.io/cluster-issuer" = var.cluster_issuer
    }
  }
  spec {
    ingress_class_name = var.ingress_class
    tls {
      hosts       = local.hosts
      secret_name = "garage-tls"
    }
    dynamic "rule" {
      for_each = merge({ (var.s3_host) = "s3" }, { for _, h in local.website_hosts : h => "web" })
      content {
        host = rule.key
        http {
          path {
            path      = "/"
            path_type = "Prefix"
            backend {
              service {
                name = kubernetes_service_v1.garage.metadata[0].name
                port {
                  name = rule.value
                }
              }
            }
          }
        }
      }
    }
  }
  # The address is ServiceLB's; nothing to wait for.
  wait_for_load_balancer = false
  depends_on             = [aws_route53_record.garage]
}

data "aws_route53_zone" "site" {
  count = var.manage_dns_records ? 1 : 0
  name  = var.dns_zone
}

resource "aws_route53_record" "garage" {
  for_each = var.manage_dns_records ? toset(local.hosts) : []
  zone_id  = data.aws_route53_zone.site[0].zone_id
  name     = each.value
  type     = "A"
  ttl      = 300
  records  = [var.node_ip]
}

# Prometheus (the monitoring root's, which watches every namespace) scrapes
# /metrics on the admin port with the metrics token.
resource "kubernetes_manifest" "service_monitor" {
  count = var.metrics ? 1 : 0
  manifest = {
    apiVersion = "monitoring.coreos.com/v1"
    kind       = "ServiceMonitor"
    metadata = {
      name      = "garage"
      namespace = local.ns
      labels    = local.labels
    }
    spec = {
      selector = { matchLabels = local.labels }
      endpoints = [{
        port     = "admin"
        path     = "/metrics"
        interval = "30s"
        authorization = {
          type = "Bearer"
          credentials = {
            name = kubernetes_secret_v1.garage.metadata[0].name
            key  = "GARAGE_METRICS_TOKEN"
          }
        }
      }]
    }
  }
}
