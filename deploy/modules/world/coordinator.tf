# The coordinator (VOXEL_ROLE=coordinator): claims, history, the successions, the
# world cuts' decisions. var.coordinator_replicas pods (default 2; Elastic 2b): each
# is a replica with an id the store issues at its start, never reused; all of them
# hold the one cache of the store's coordinator row and write it by a compare-and-set
# on its version, which is the only thing that keeps their writes apart (not this
# count, the budget or the strategy). The lowest live replica id is the ticker (the
# timed and lease work; mmo_coordinator_is_ticker). Every replica mounts the same
# identity Secrets (identities.tf): one pin for all of them, so a host or the login
# service dialing coordinator.<ns> (the headless Service below resolves to every pod)
# reaches any replica. Each listens for hosts on 6480/UDP and for the login service on
# 6490/UDP, and serves /livez, /readyz (200 once its join committed) and /metrics on 9100.
#
# Recreate: a template change (an image set by a release) stops every replica before
# the new ones start, so no two builds ever share the row. Releases stop the world
# first anyway (world_update.sh scales the coordinator to 0, sets the images, scales it
# back to the count). The PodDisruptionBudget (min_available 1, only above one replica)
# keeps one replica up through a voluntary eviction on a cluster with room to move it;
# on ONE node `kubectl drain` cannot finish while the world runs: `just world-stop`
# first (hosts.tf). world-stop records the count in the annotation errantry/replicas
# before it scales to 0, and world-start restores it (absent: the module default).

resource "kubernetes_deployment_v1" "coordinator" {
  metadata {
    name      = "coordinator"
    namespace = local.ns
    labels    = local.coordinator_labels
  }
  spec {
    replicas = var.coordinator_replicas
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
        volume {
          name = "login-link-identity"
          secret {
            secret_name  = kubernetes_secret_v1.link_identity["login-link"].metadata[0].name
            default_mode = local.secret_mode
          }
        }
        container {
          name              = "coordinator"
          image             = var.images.server
          image_pull_policy = "IfNotPresent"
          dynamic "env" {
            for_each = merge(local.server_env, {
              VOXEL_ROLE                      = "coordinator"
              VOXEL_COORDINATOR_BIND          = "0.0.0.0:6480"
              VOXEL_LOGIN_LINK_BIND           = "0.0.0.0:6490"
              VOXEL_LOGIN_LINK_CERT_HASH_FILE = "${local.login_link_dir}/${local.link_file}"
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
            name           = "login"
            container_port = 6490
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
          volume_mount {
            name       = "login-link-identity"
            mount_path = local.login_link_dir
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
  # Releases set the image (world_update.sh); an apply must not undo them. The
  # Deployment's own annotations are world-stop's record of the replica count
  # (errantry/replicas); none is set here.
  lifecycle {
    ignore_changes = [
      spec[0].template[0].spec[0].container[0].image,
      metadata[0].annotations,
    ]
  }
}

# A voluntary eviction (a node drain) leaves at least one replica running: see the header.
resource "kubernetes_pod_disruption_budget_v1" "coordinator" {
  count = var.coordinator_replicas > 1 ? 1 : 0
  metadata {
    name      = "coordinator"
    namespace = local.ns
    labels    = local.coordinator_labels
  }
  spec {
    min_available = "1"
    selector {
      match_labels = local.coordinator_labels
    }
  }
}

# Headless: hosts (6480) and the login service (6490, output login_link) resolve
# coordinator.<ns>.svc.cluster.local to every replica pod's own IP at every dial, so no UDP
# conntrack entry to a Service IP outlives a restart, and an unready coordinator
# (waiting for its forge) is still reachable. The health port is here for the
# ServiceMonitor (charts/world-monitor).
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
      name        = "login"
      port        = 6490
      target_port = "login"
      protocol    = "UDP"
    }
    port {
      name        = "health"
      port        = local.health_port
      target_port = "health"
    }
  }
}
