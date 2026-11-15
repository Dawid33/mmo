# The browser bundle (mmo/web: nginx-unprivileged on 8080, /healthz, a /login
# proxy to LOGIN_UPSTREAM) behind the cluster's Traefik, when var.web is set.
# TLS: with var.client_certificate set, the Ingress gets a certificate from the
# same ClusterIssuer (cert-manager annotation, as the login module's Ingress); with
# it null (a cluster without cert-manager) the Ingress is plain HTTP.

locals {
  web_tls = var.web != null && var.client_certificate != null
}

resource "kubernetes_deployment_v1" "web" {
  count = var.web == null ? 0 : 1
  metadata {
    name      = "web"
    namespace = local.ns
    labels    = local.web_labels
  }
  spec {
    replicas = 2
    strategy {
      type = "RollingUpdate"
      rolling_update {
        max_unavailable = 0
        max_surge       = 1
      }
    }
    selector {
      match_labels = local.web_labels
    }
    template {
      metadata {
        labels = local.web_labels
      }
      spec {
        automount_service_account_token  = false
        enable_service_links             = false
        termination_grace_period_seconds = 30
        dynamic "image_pull_secrets" {
          for_each = var.image_pull_secret == null ? [] : [var.image_pull_secret]
          content {
            name = image_pull_secrets.value
          }
        }
        # The image's own user (nginx-unprivileged, uid 101).
        security_context {
          run_as_non_root = true
        }
        container {
          name              = "web"
          image             = var.images.web
          image_pull_policy = "IfNotPresent"
          env {
            name  = "LOGIN_UPSTREAM"
            value = var.web.login_upstream
          }
          port {
            name           = "http"
            container_port = 8080
          }
          # Traefik needs a few seconds to stop routing to a deleted pod.
          lifecycle {
            pre_stop {
              exec {
                command = ["sleep", "5"]
              }
            }
          }
          security_context {
            allow_privilege_escalation = false
            capabilities {
              drop = ["ALL"]
            }
          }
          resources {
            requests = { cpu = "10m", memory = "32Mi" }
            limits   = { memory = "128Mi" }
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
  # Releases set the image (world_update.sh); an apply must not undo them.
  lifecycle {
    ignore_changes = [spec[0].template[0].spec[0].container[0].image]
  }
}

resource "kubernetes_service_v1" "web" {
  count = var.web == null ? 0 : 1
  metadata {
    name      = "web"
    namespace = local.ns
    labels    = local.web_labels
  }
  spec {
    selector = local.web_labels
    port {
      name        = "http"
      port        = 80
      target_port = "http"
    }
  }
}

resource "kubernetes_ingress_v1" "web" {
  count = var.web == null ? 0 : 1
  metadata {
    name        = "web"
    namespace   = local.ns
    labels      = local.web_labels
    annotations = local.web_tls ? { "cert-manager.io/cluster-issuer" = var.client_certificate.issuer } : {}
  }
  spec {
    ingress_class_name = "traefik"
    dynamic "tls" {
      for_each = local.web_tls ? [var.web.ingress_host] : []
      content {
        hosts       = [tls.value]
        secret_name = "web-tls"
      }
    }
    rule {
      host = var.web.ingress_host
      http {
        path {
          path      = "/"
          path_type = "Prefix"
          backend {
            service {
              name = kubernetes_service_v1.web[0].metadata[0].name
              port {
                name = "http"
              }
            }
          }
        }
      }
    }
  }
  # The address is ServiceLB's; nothing to wait for.
  wait_for_load_balancer = false
}
