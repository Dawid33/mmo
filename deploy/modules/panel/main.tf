# The world panel (crates/panel; plan docs/superpowers/plans/2026-11-15-r5b-world-panel.md):
# one pod serving the operator page on a ClusterIP Service — NO Ingress. It is reached by
# port-forward only (`just orion-panel`): whoever may port-forward into this namespace may
# use it, and it has no login of its own.
#
# Permissions are per world. This module makes:
#   - ClusterRole `world-panel`: what the panel may do INSIDE a world namespace (read its
#     pods, logs, ConfigMaps, Deployments, StatefulSet and Jobs, ask its pods' health port
#     through the API server's pod proxy, scale the coordinator and the hosts, create forge
#     Jobs). It is bound in each world's namespace by the world module (`var.panel`), never
#     cluster-wide.
#   - ClusterRole + ClusterRoleBinding `panel-namespaces`: list and get namespaces (how the
#     panel finds the worlds: the label app.kubernetes.io/name=world).
# Creating a Job lets the panel run the server image with the world's `world-db` Secret —
# that is the forge's purpose, and why the binding is per world. The panel never reads a
# Secret itself.

locals {
  labels = {
    "app.kubernetes.io/name"      = "panel"
    "app.kubernetes.io/component" = "panel"
  }
  uid = 10001
}

resource "kubernetes_namespace_v1" "panel" {
  metadata {
    name   = var.namespace
    labels = { "app.kubernetes.io/name" = "panel" }
  }
}

resource "kubernetes_service_account_v1" "panel" {
  metadata {
    name      = "panel"
    namespace = kubernetes_namespace_v1.panel.metadata[0].name
    labels    = local.labels
  }
  automount_service_account_token = false
}

resource "kubernetes_cluster_role_v1" "world_panel" {
  metadata {
    name   = "world-panel"
    labels = local.labels
  }
  rule {
    api_groups = [""]
    resources  = ["pods", "configmaps"]
    verbs      = ["get", "list"]
  }
  rule {
    api_groups = [""]
    resources  = ["pods/log"]
    verbs      = ["get"]
  }
  # /readyz and /metrics of the coordinator and hosts (health port 9100), as `just world-status`.
  rule {
    api_groups = [""]
    resources  = ["pods/proxy"]
    verbs      = ["get"]
  }
  rule {
    api_groups = ["apps"]
    resources  = ["deployments", "statefulsets"]
    verbs      = ["get", "list"]
  }
  rule {
    api_groups = ["apps"]
    resources  = ["deployments/scale", "statefulsets/scale"]
    verbs      = ["get", "patch", "update"]
  }
  rule {
    api_groups = ["batch"]
    resources  = ["jobs"]
    verbs      = ["get", "list", "create"]
  }
}

resource "kubernetes_cluster_role_v1" "namespaces" {
  metadata {
    name   = "panel-namespaces"
    labels = local.labels
  }
  rule {
    api_groups = [""]
    resources  = ["namespaces"]
    verbs      = ["get", "list"]
  }
}

resource "kubernetes_cluster_role_binding_v1" "namespaces" {
  metadata {
    name   = "panel-namespaces"
    labels = local.labels
  }
  role_ref {
    api_group = "rbac.authorization.k8s.io"
    kind      = "ClusterRole"
    name      = kubernetes_cluster_role_v1.namespaces.metadata[0].name
  }
  subject {
    kind      = "ServiceAccount"
    name      = kubernetes_service_account_v1.panel.metadata[0].name
    namespace = kubernetes_namespace_v1.panel.metadata[0].name
  }
}

resource "kubernetes_deployment_v1" "panel" {
  metadata {
    name      = "panel"
    namespace = kubernetes_namespace_v1.panel.metadata[0].name
    labels    = local.labels
  }
  spec {
    # One: a start or stop runs inside the pod that was asked (its log lives there too).
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
      }
      spec {
        service_account_name = kubernetes_service_account_v1.panel.metadata[0].name
        # The panel's one credential: the API server's ServiceAccount token.
        automount_service_account_token  = true
        enable_service_links             = false
        termination_grace_period_seconds = 10
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
        }
        container {
          name              = "panel"
          image             = var.image
          image_pull_policy = "IfNotPresent"
          env {
            name  = "PANEL_BIND"
            value = "0.0.0.0:8080"
          }
          dynamic "env" {
            for_each = var.grafana_url == null ? [] : [var.grafana_url]
            content {
              name  = "PANEL_GRAFANA_URL"
              value = env.value
            }
          }
          port {
            name           = "http"
            container_port = 8080
          }
          security_context {
            allow_privilege_escalation = false
            read_only_root_filesystem  = true
            capabilities {
              drop = ["ALL"]
            }
          }
          resources {
            requests = { cpu = "10m", memory = var.memory }
            limits   = { memory = var.memory }
          }
          readiness_probe {
            http_get {
              path = "/healthz"
              port = "http"
            }
            period_seconds = 10
          }
          liveness_probe {
            http_get {
              path = "/healthz"
              port = "http"
            }
            period_seconds = 30
          }
        }
      }
    }
  }
  # Releases set the image (scripts/orion_panel_push.sh); an apply must not undo them.
  lifecycle {
    ignore_changes = [spec[0].template[0].spec[0].container[0].image]
  }
}

resource "kubernetes_service_v1" "panel" {
  metadata {
    name      = "panel"
    namespace = kubernetes_namespace_v1.panel.metadata[0].name
    labels    = local.labels
  }
  spec {
    selector = local.labels
    port {
      name        = "http"
      port        = 80
      target_port = "http"
    }
  }
}
