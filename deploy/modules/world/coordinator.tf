# The coordinator (VOXEL_ROLE=coordinator): claims, embark/return, history, the
# successions, the world cuts' decisions. One pod, Recreate: two coordinators of
# one world must never run side by side. It listens for its hosts on 6480/UDP with
# the identity from identities.tf, and serves /livez, /readyz and /metrics on 9100.

resource "kubernetes_deployment_v1" "coordinator" {
  metadata {
    name      = "coordinator"
    namespace = local.ns
    labels    = local.coordinator_labels
  }
  spec {
    replicas = 1
    strategy {
      type = "Recreate"
    }
    selector {
      match_labels = local.coordinator_labels
    }
    template {
      metadata {
        labels = local.coordinator_labels
        annotations = {
          "checksum/secrets" = local.coordinator_checksum
        }
      }
      spec {
        automount_service_account_token  = false
        enable_service_links             = false
        termination_grace_period_seconds = 60
        dynamic "image_pull_secrets" {
          for_each = var.image_pull_secret == null ? [] : [var.image_pull_secret]
          content {
            name = image_pull_secrets.value
          }
        }
        security_context {
          run_as_user     = local.uid
          run_as_group    = local.uid
          run_as_non_root = true
          fs_group        = local.uid
        }
        volume {
          name = "service"
          secret {
            secret_name  = kubernetes_secret_v1.service.metadata[0].name
            default_mode = local.secret_mode
          }
        }
        volume {
          name = "coordinator-identity"
          secret {
            secret_name  = kubernetes_secret_v1.link_identity["coordinator"].metadata[0].name
            default_mode = local.secret_mode
          }
        }
        container {
          name              = "coordinator"
          image             = var.images.server
          image_pull_policy = "IfNotPresent"
          dynamic "env" {
            for_each = merge(local.server_env, {
              VOXEL_ROLE             = "coordinator"
              VOXEL_COORDINATOR_BIND = "0.0.0.0:6480"
            })
            content {
              name  = env.key
              value = env.value
            }
          }
          env {
            name = "VOXEL_WORLD_DB"
            value_from {
              secret_key_ref {
                name = kubernetes_secret_v1.db.metadata[0].name
                key  = "url"
              }
            }
          }
          port {
            name           = "link"
            container_port = 6480
            protocol       = "UDP"
          }
          port {
            name           = "health"
            container_port = local.health_port
          }
          volume_mount {
            name       = "service"
            mount_path = local.service_token_dir
            read_only  = true
          }
          volume_mount {
            name       = "coordinator-identity"
            mount_path = local.coordinator_dir
            read_only  = true
          }
          security_context {
            allow_privilege_escalation = false
            capabilities {
              drop = ["ALL"]
            }
          }
          resources {
            requests = local.coordinator_resources.requests
            limits   = local.coordinator_resources.limits
          }
          # No startup probe. /livez fails only after LIVEZ_STALL_MS (30 s) of a
          # stalled registered loop, so this fires after about a minute.
          liveness_probe {
            http_get {
              path = "/livez"
              port = "health"
            }
            period_seconds    = 10
            failure_threshold = 3
          }
          readiness_probe {
            http_get {
              path = "/readyz"
              port = "health"
            }
            period_seconds = 5
          }
        }
      }
    }
  }
  # Unready until the world is forged (main.tf): never wait for the rollout.
  wait_for_rollout = false
  # Releases set the image (world_update.sh); an apply must not undo them.
  lifecycle {
    ignore_changes = [spec[0].template[0].spec[0].container[0].image]
  }
}

# Headless: hosts resolve coordinator.<ns>.svc.cluster.local to the pod's own IP
# at every dial, so no UDP conntrack entry to a Service IP outlives a restart, and
# an unready coordinator (waiting for its forge) is still reachable. The health
# port is here for the ServiceMonitor (charts/world-monitor).
resource "kubernetes_service_v1" "coordinator" {
  metadata {
    name      = "coordinator"
    namespace = local.ns
    labels    = local.coordinator_labels
  }
  spec {
    cluster_ip                  = "None"
    publish_not_ready_addresses = true
    selector                    = local.coordinator_labels
    port {
      name        = "link"
      port        = 6480
      target_port = "link"
      protocol    = "UDP"
    }
    port {
      name        = "health"
      port        = local.health_port
      target_port = "health"
    }
  }
}
