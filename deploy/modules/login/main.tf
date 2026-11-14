# The game's login service (`login serve`, crates/login) at https://<host>:
# accounts' refresh tokens, the signed join tickets and the JWKS the game hosts
# verify them with. Keycloak (modules/keycloak) owns the passwords; this service
# trades a Keycloak access token for its own refresh token. Its own PostgreSQL is
# in postgres.tf.
#
# The image is mmo/login in ECR (the registry root; deploy/images/login/Dockerfile:
# debian + the release binary at /usr/local/bin/login, entrypoint `login`). The
# Deployment is created with var.image_tag (`bootstrap`, pushed by the FIRST
# `just orion-login-push`) and then ignores its image: releases are
# `kubectl set image` to <repo>@sha256:<digest> (never a tag: a re-pushed tag
# would not roll) by `just orion-login-push`, rolled back with
# `kubectl rollout undo`, and a later apply never rolls a release back. Push
# BEFORE the first apply: the Deployment waits for its rollout, so without the
# image the apply blocks about 10 minutes, fails and taints the Deployment.
# Pulling needs a token (ecr.tf): Secret login-ecr, made here at apply time and
# rewritten every 6 hours by the CronJob ecr-refresh.
#
# Two replicas (var.replicas) protect against a deploy or a crashed pod: with
# max_unavailable = 0 a new pod must be Ready (its health check answering, which
# needs the database and Keycloak's discovery at start; afterwards /healthz is
# static) before an old one stops, and a PodDisruptionBudget (min_available = 1)
# keeps one up during drains. That budget makes `kubectl drain` of this single
# node wait on the second pod; a reboot without a drain is unaffected. The pods do
# NOT protect against the node going down or the single PostgreSQL going down:
# both stay single points.
#
# Public: only /v1/ and /.well-known/jwks.json. NOT public: the health check and
# /metrics (the ServiceMonitor scrapes the Service inside the cluster).
#
# The signing key is generated here (ED25519, PKCS#8 PEM) and lives in the state:
# every *.pem in the mounted directory is published, the last by file name signs.
# Rotation: add a second key as 0002.pem in a later change, and add it to the
# `checksum/secrets` annotation too, or the pods will not roll when it changes.
# Never replace tls_private_key.signing: that invalidates the join tickets signed
# in the last 5 minutes (JOIN_JWT_TTL_SECS = 300 in crates/login-api); refresh
# tokens are database rows and not affected.

locals {
  ns     = kubernetes_namespace_v1.login.metadata[0].name
  labels = { "app.kubernetes.io/name" = "login" }
  port   = 8790
}

resource "kubernetes_namespace_v1" "login" {
  metadata {
    name = var.namespace
  }
}

resource "random_password" "service_token" {
  length  = 48
  special = false
}

resource "kubernetes_secret_v1" "service" {
  metadata {
    name      = "login-service"
    namespace = local.ns
  }
  data = {
    "service-token" = random_password.service_token.result
  }
}

resource "tls_private_key" "signing" {
  algorithm = "ED25519"
}

resource "kubernetes_secret_v1" "keys" {
  metadata {
    name      = "login-keys"
    namespace = local.ns
  }
  data = {
    "0001.pem" = tls_private_key.signing.private_key_pem
  }
}

resource "kubernetes_deployment_v1" "login" {
  metadata {
    name      = "login"
    namespace = local.ns
    labels    = local.labels
  }
  spec {
    replicas = var.replicas
    strategy {
      type = "RollingUpdate"
      rolling_update {
        max_unavailable = 0
        max_surge       = 1
      }
    }
    selector {
      match_labels = local.labels
    }
    template {
      metadata {
        labels = local.labels
        annotations = {
          # A changed secret rolls the pods (env and files are read at start).
          # Not the pull secret (ecr.tf): it changes every 6 hours and is read
          # by the kubelet, not by the pods.
          "checksum/secrets" = sha256(join(",", [
            random_password.db.result,
            random_password.service_token.result,
            tls_private_key.signing.private_key_pem,
          ]))
        }
      }
      spec {
        automount_service_account_token = false
        enable_service_links            = false
        # Does not depend on the CronJob: the Secret is filled at apply time.
        image_pull_secrets {
          name = kubernetes_secret_v1.ecr_pull.metadata[0].name
        }
        # On deletion the endpoint is removed from the Service and the preStop
        # sleep runs at the same time; Traefik needs a few seconds to stop routing
        # to the pod, and the service refuses connections once it gets SIGTERM.
        # In-flight requests then finish within the grace period (the sleep counts
        # against it).
        termination_grace_period_seconds = 30
        dynamic "host_aliases" {
          for_each = var.issuer_host_alias_ip == null ? [] : [var.issuer_host_alias_ip]
          content {
            ip        = host_aliases.value
            hostnames = [regex("^[a-z]+://([^/:]+)", var.issuer)[0]]
          }
        }
        volume {
          name = "keys"
          secret {
            secret_name  = kubernetes_secret_v1.keys.metadata[0].name
            default_mode = "0400"
          }
        }
        volume {
          name = "service"
          secret {
            secret_name  = kubernetes_secret_v1.service.metadata[0].name
            default_mode = "0400"
          }
        }
        container {
          name  = "login"
          image = "${var.image_repository}:${var.image_tag}"
          # The image's entrypoint is the binary itself (`login`), not a shell.
          args              = ["serve"]
          image_pull_policy = "IfNotPresent"
          env {
            name  = "VOXEL_LOGIN_BIND"
            value = "0.0.0.0:${local.port}"
          }
          env {
            name  = "VOXEL_LOGIN_KEYS_DIR"
            value = "/keys"
          }
          env {
            name  = "VOXEL_LOGIN_CORS_ORIGINS"
            value = join(",", var.cors_origins)
          }
          env {
            name  = "VOXEL_OIDC_ISSUER"
            value = var.issuer
          }
          env {
            name  = "VOXEL_OIDC_AUDIENCE"
            value = var.oidc_audience
          }
          env {
            name  = "VOXEL_OIDC_CLIENT_ID"
            value = var.oidc_client_id
          }
          env {
            name  = "VOXEL_SERVICE_TOKEN_FILE"
            value = "/run/login/service-token"
          }
          # ORDER MATTERS: Kubernetes expands $(PGPASSWORD) below only because
          # PGPASSWORD is declared BEFORE VOXEL_LOGIN_DB in this list. The
          # password has no special characters (no URL-encoding), and it never
          # appears in the Deployment spec.
          env {
            name = "PGPASSWORD"
            value_from {
              secret_key_ref {
                name = kubernetes_secret_v1.db.metadata[0].name
                key  = "password"
              }
            }
          }
          env {
            name  = "VOXEL_LOGIN_DB"
            value = "postgres://login:$(PGPASSWORD)@${kubernetes_service_v1.postgres.metadata[0].name}:5432/login"
          }
          port {
            name           = "http"
            container_port = local.port
          }
          lifecycle {
            pre_stop {
              exec {
                command = ["sleep", "5"]
              }
            }
          }
          volume_mount {
            name       = "keys"
            mount_path = "/keys"
            read_only  = true
          }
          volume_mount {
            name       = "service"
            mount_path = "/run/login"
            read_only  = true
          }
          resources {
            requests = { cpu = "10m", memory = "32Mi" }
            limits   = { memory = var.memory_limits["login"] }
          }
          readiness_probe {
            http_get {
              path = "/healthz"
              port = "http"
            }
            period_seconds = 10
          }
          liveness_probe {
            tcp_socket {
              port = "http"
            }
            initial_delay_seconds = 10
            period_seconds        = 30
          }
        }
      }
    }
  }
  # Releases set the image (`just orion-login-push`); an apply must not undo them.
  lifecycle {
    ignore_changes = [spec[0].template[0].spec[0].container[0].image]
  }
  depends_on = [kubernetes_stateful_set_v1.postgres]
}

resource "kubernetes_pod_disruption_budget_v1" "login" {
  count = var.replicas > 1 ? 1 : 0
  metadata {
    name      = "login"
    namespace = local.ns
  }
  spec {
    min_available = "1"
    selector {
      match_labels = local.labels
    }
  }
}

# The labels are what the ServiceMonitor (helm_release.monitor) selects, and the
# port must be named `http`.
resource "kubernetes_service_v1" "login" {
  metadata {
    name      = "login"
    namespace = local.ns
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

resource "kubernetes_ingress_v1" "login" {
  metadata {
    name      = "login"
    namespace = local.ns
    annotations = {
      "cert-manager.io/cluster-issuer" = var.cluster_issuer
    }
  }
  spec {
    ingress_class_name = var.ingress_class
    tls {
      hosts       = [var.host]
      secret_name = "login-tls"
    }
    rule {
      host = var.host
      http {
        # Only the public API and the key set. Nothing routes /, the health
        # check or /metrics.
        dynamic "path" {
          for_each = ["/v1/", "/.well-known/jwks.json"]
          content {
            path      = path.value
            path_type = "Prefix"
            backend {
              service {
                name = kubernetes_service_v1.login.metadata[0].name
                port {
                  name = "http"
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
  depends_on             = [aws_route53_record.login]
}

data "aws_route53_zone" "site" {
  count = var.manage_dns_record ? 1 : 0
  name  = var.dns_zone
}

resource "aws_route53_record" "login" {
  count   = var.manage_dns_record ? 1 : 0
  zone_id = data.aws_route53_zone.site[0].zone_id
  name    = var.host
  type    = "A"
  ttl     = 300
  records = [var.node_ip]
}

# A ServiceMonitor is a custom resource, so it is a tiny local chart. The
# monitoring root's Prometheus picks up ServiceMonitors from every namespace.
resource "helm_release" "monitor" {
  count     = var.metrics ? 1 : 0
  name      = "login-monitor"
  namespace = local.ns
  chart     = "${path.module}/charts/login-monitor"

  values = [yamlencode({
    selectorLabels = local.labels
  })]

  depends_on = [kubernetes_service_v1.login]
}
