# The reconciler (VOXEL_ROLE=fleet; crates/server/src/fleet.rs): every 2 s it reads the
# table desired_hosts (a read-only store handle) and makes the world's namespace match it —
# one Deployment, Service and PodDisruptionBudget host-<name> per row, rendered from the
# ConfigMap host-template (hosts.tf), and nothing labelled errantry/fleet=<world id> for a
# name without a row. It never talks to the coordinator, and the coordinator never talks to
# the Kubernetes API: the coordinator's decisions write desired_hosts, this pod follows.
#
# It creates and re-templates nothing while the coordinator Deployment is at 0 replicas
# (world-stop), and it never changes an existing Deployment's replicas or image: those
# belong to world-stop / world-start / scripts/world_update.sh. A new host gets the image
# of the coordinator container at that moment.
#
# Exactly ONE replica, strategy Recreate: two reconcilers overlapping (a rolling update, a
# second replica) could each act on a view the other just changed — one deleting a host the
# other has just created for a fresh row. Recreate stops the old pod before the new one
# starts. Nothing here keeps two writers of the STORE apart (the store's fencing does); this
# only keeps the cluster's objects from flapping.
#
# Unready while the world is not forged (/readyz 503 `forge`), while the coordinator is at 0
# (`stopped`) and after failed passes (`failing`): nothing waits on its readiness.
# /metrics (role="fleet"): mmo_fleet_desired, mmo_fleet_deployments, mmo_fleet_pass_age_ms,
# mmo_fleet_{created,deleted,errors}_total.

locals {
  fleet_labels = merge(local.labels, { "app.kubernetes.io/component" = "fleet" })
}

# Its API token is mounted only in the fleet pod (the account's default is off).
resource "kubernetes_service_account_v1" "fleet" {
  metadata {
    name      = "fleet"
    namespace = local.ns
    labels    = local.fleet_labels
  }
  automount_service_account_token = false
}

# Namespaced: the reconciler touches only its own world's namespace. No patch, no scale
# subresource, no pods. Deployments: get (the coordinator's replicas and image), list,
# create, update (a re-template at 0 replicas), delete.
resource "kubernetes_role_v1" "fleet" {
  metadata {
    name      = "fleet"
    namespace = local.ns
    labels    = local.fleet_labels
  }
  rule {
    api_groups = ["apps"]
    resources  = ["deployments"]
    verbs      = ["get", "list", "create", "update", "delete"]
  }
  rule {
    api_groups = [""]
    resources  = ["services"]
    verbs      = ["get", "list", "create", "delete"]
  }
  rule {
    api_groups = ["policy"]
    resources  = ["poddisruptionbudgets"]
    verbs      = ["get", "list", "create", "delete"]
  }
  rule {
    api_groups     = [""]
    resources      = ["configmaps"]
    resource_names = [kubernetes_config_map_v1.host_template.metadata[0].name]
    verbs          = ["get"]
  }
}

resource "kubernetes_role_binding_v1" "fleet" {
  metadata {
    name      = "fleet"
    namespace = local.ns
    labels    = local.fleet_labels
  }
  role_ref {
    api_group = "rbac.authorization.k8s.io"
    kind      = "Role"
    name      = kubernetes_role_v1.fleet.metadata[0].name
  }
  subject {
    kind      = "ServiceAccount"
    name      = kubernetes_service_account_v1.fleet.metadata[0].name
    namespace = local.ns
  }
}

resource "kubernetes_deployment_v1" "fleet" {
  metadata {
    name      = "fleet"
    namespace = local.ns
    labels    = local.fleet_labels
  }
  spec {
    # ONE reconciler, never two at once: see the header.
    replicas = 1
    strategy {
      type = "Recreate"
    }
    selector {
      match_labels = local.fleet_labels
    }
    template {
      metadata {
        labels = local.fleet_labels
        annotations = {
          "checksum/secrets" = sha256(var.database_url)
        }
      }
      spec {
        service_account_name            = kubernetes_service_account_v1.fleet.metadata[0].name
        automount_service_account_token = true
        enable_service_links            = false
        # A pass's requests are bounded by 15 s each; a stop between passes exits at once.
        termination_grace_period_seconds = 30
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
        container {
          name              = "fleet"
          image             = var.images.server
          image_pull_policy = "IfNotPresent"
          # VOXEL_FLEET_KUBE_API stays unset: the in-cluster API server, this pod's token.
          env {
            name = "VOXEL_FLEET_NAMESPACE"
            value_from {
              field_ref {
                field_path = "metadata.namespace"
              }
            }
          }
          dynamic "env" {
            for_each = merge(local.telemetry_env, {
              VOXEL_ROLE              = "fleet"
              VOXEL_WORLD_ID          = var.world_id
              VOXEL_FLEET_PORT_BASE   = tostring(var.fleet.port_base)
              VOXEL_HEALTH_BIND       = "0.0.0.0:${local.health_port}"
              OTEL_TRACES_SAMPLER_ARG = tostring(var.telemetry.fleet_sampler_ratio)
            })
            content {
              name  = env.key
              value = env.value
            }
          }
          env {
            name = "K8S_POD_NAME"
            value_from {
              field_ref {
                field_path = "metadata.name"
              }
            }
          }
          env {
            name = "K8S_NAMESPACE_NAME"
            value_from {
              field_ref {
                field_path = "metadata.namespace"
              }
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
            name           = "health"
            container_port = local.health_port
          }
          security_context {
            allow_privilege_escalation = false
            capabilities {
              drop = ["ALL"]
            }
          }
          resources {
            requests = { cpu = "10m", memory = "64Mi" }
            limits   = { memory = "256Mi" }
          }
          # /livez fails only after LIVEZ_STALL_MS (30 s) of a stalled loop (it beats
          # after every API request).
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
  # Unready until the world is forged and whenever it is stopped: never wait for the rollout.
  wait_for_rollout = false
  # Releases set the image (world_update.sh); an apply must not undo them.
  lifecycle {
    ignore_changes = [spec[0].template[0].spec[0].container[0].image]
  }
}

# Headless, for the ServiceMonitor only (charts/world-monitor).
resource "kubernetes_service_v1" "fleet" {
  metadata {
    name      = "fleet"
    namespace = local.ns
    labels    = local.fleet_labels
  }
  spec {
    cluster_ip                  = "None"
    publish_not_ready_addresses = true
    selector                    = local.fleet_labels
    port {
      name        = "health"
      port        = local.health_port
      target_port = "health"
    }
  }
}
