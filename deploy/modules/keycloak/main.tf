# Keycloak (identity provider) at https://<host>: where player accounts live
# (sign-up, email verification, password reset) and what Zulip and the game
# log in against over OpenID Connect. One pod + its own PostgreSQL (postgres.tf).
# Realms, clients and mail are configured by the keycloak-realm root, through
# the admin API.
#
# Public: only /realms/, /resources/ and /.well-known/ (what logins need).
# NOT public: the admin console and admin API (/admin/), health and metrics
# (management port 9000, never routed). Admin access is a port-forward:
# `just orion-keycloak-admin` -> http://127.0.0.1:<admin_port>/admin/.
# The `master` realm's LOGIN pages (/realms/master/) are public and have to be:
# the admin console, wherever it is opened, signs its user in against the public
# address (KC_HOSTNAME). Blocking them breaks it ("Timeout when waiting for 3rd
# party check iframe message"). What guards the admin is its 32-character
# random password, and that a token is useless from outside (no /admin/ route).
#
# The account console (/realms/<realm>/account/) and its API are switched off
# (KC_FEATURES_DISABLED): players manage their profile on the site's own page, and
# an unused, publicly routed console is attack surface with the stock look.
#
# Folder login themes (var.themes) are mounted from ConfigMaps under
# /opt/keycloak/themes/<name>. The realm picks one by name (keycloak-realm root).

locals {
  ns        = kubernetes_namespace_v1.keycloak.metadata[0].name
  labels    = { "app.kubernetes.io/name" = "keycloak" }
  admin_url = "http://127.0.0.1:${var.admin_port}"

  env = merge({
    KC_DB          = "postgres"
    KC_DB_URL      = "jdbc:postgresql://${kubernetes_service_v1.postgres.metadata[0].name}:5432/keycloak"
    KC_DB_USERNAME = "keycloak"
    # The address tokens are issued for, and the one the admin console insists on.
    KC_HOSTNAME       = "https://${var.host}"
    KC_HOSTNAME_ADMIN = local.admin_url
    # Traefik terminates TLS and speaks plain HTTP to the pod.
    KC_PROXY_HEADERS            = "xforwarded"
    KC_HTTP_ENABLED             = "true"
    KC_HEALTH_ENABLED           = "true"
    KC_METRICS_ENABLED          = "true"
    KC_BOOTSTRAP_ADMIN_USERNAME = "admin"
    # Read by the orion theme: where its "back to the site" link points.
    ORION_SITE_URL = var.site_url
    }, length(var.features_disabled) == 0 ? {} : {
    KC_FEATURES_DISABLED = join(",", var.features_disabled)
  })
  secret_env = {
    KC_DB_PASSWORD              = { name = kubernetes_secret_v1.db.metadata[0].name, key = "password" }
    KC_BOOTSTRAP_ADMIN_PASSWORD = { name = kubernetes_secret_v1.admin.metadata[0].name, key = "password" }
  }
}

resource "kubernetes_namespace_v1" "keycloak" {
  metadata {
    name = var.namespace
  }
}

# The `master` realm's admin, created on the first start only: replacing this
# afterwards does not change the password in the database.
resource "random_password" "admin" {
  length  = 32
  special = false
}

resource "kubernetes_secret_v1" "admin" {
  metadata {
    name      = "keycloak-admin"
    namespace = local.ns
  }
  data = {
    password = random_password.admin.result
  }
}

# One ConfigMap per theme. Keys may only hold [-._a-zA-Z0-9], so a path's `/`
# becomes `__`; the volume's `items` restore the real paths under the mount.
resource "kubernetes_config_map_v1" "theme" {
  for_each = var.themes
  metadata {
    name      = "keycloak-theme-${each.key}"
    namespace = local.ns
    labels    = local.labels
  }
  data = { for path, content in each.value : replace(path, "/", "__") => content }

  lifecycle {
    # A ConfigMap is capped at 1 MiB; stay clear of it.
    precondition {
      condition     = sum([for content in values(each.value) : length(content)]) < 900000
      error_message = "Theme ${each.key} is over 900000 bytes of content; a ConfigMap holds 1 MiB."
    }
  }
}

resource "kubernetes_deployment_v1" "keycloak" {
  metadata {
    name      = "keycloak"
    namespace = local.ns
    labels    = local.labels
  }
  spec {
    replicas = 1
    # One node of an unclustered Keycloak at a time.
    strategy {
      type = "Recreate"
    }
    selector {
      match_labels = local.labels
    }
    template {
      metadata {
        labels = local.labels
        # Keycloak caches themes, so a changed theme needs a new pod.
        annotations = {
          "checksum/themes" = sha256(jsonencode(var.themes))
        }
      }
      spec {
        automount_service_account_token = false
        enable_service_links            = false
        dynamic "volume" {
          for_each = kubernetes_config_map_v1.theme
          content {
            name = "theme-${volume.key}"
            config_map {
              name = volume.value.metadata[0].name
              dynamic "items" {
                for_each = keys(var.themes[volume.key])
                content {
                  key  = replace(items.value, "/", "__")
                  path = items.value
                }
              }
            }
          }
        }
        container {
          name  = "keycloak"
          image = var.image
          args  = ["start"]
          dynamic "env" {
            for_each = local.env
            content {
              name  = env.key
              value = env.value
            }
          }
          dynamic "env" {
            for_each = local.secret_env
            content {
              name = env.key
              value_from {
                secret_key_ref {
                  name = env.value.name
                  key  = env.value.key
                }
              }
            }
          }
          dynamic "volume_mount" {
            for_each = kubernetes_config_map_v1.theme
            content {
              name       = "theme-${volume_mount.key}"
              mount_path = "/opt/keycloak/themes/${volume_mount.key}"
              read_only  = true
            }
          }
          port {
            name           = "http"
            container_port = 8080
          }
          port {
            name           = "management"
            container_port = 9000
          }
          resources {
            requests = { cpu = "100m", memory = "768Mi" }
            limits   = { memory = var.memory_limits["keycloak"] }
          }
          # The first start builds the server and creates the schema: minutes.
          startup_probe {
            http_get {
              path = "/health/started"
              port = "management"
            }
            period_seconds    = 5
            failure_threshold = 120
          }
          readiness_probe {
            http_get {
              path = "/health/ready"
              port = "management"
            }
            period_seconds = 10
          }
          liveness_probe {
            http_get {
              path = "/health/live"
              port = "management"
            }
            period_seconds    = 30
            failure_threshold = 3
          }
        }
      }
    }
  }
  timeouts {
    create = "15m"
    update = "15m"
  }
  depends_on = [kubernetes_stateful_set_v1.postgres]
}

resource "kubernetes_service_v1" "keycloak" {
  metadata {
    name      = "keycloak"
    namespace = local.ns
    labels    = local.labels
  }
  spec {
    selector = local.labels
    port {
      name        = "http"
      port        = 8080
      target_port = "http"
    }
    port {
      name        = "management"
      port        = 9000
      target_port = "management"
    }
  }
}

resource "kubernetes_ingress_v1" "keycloak" {
  metadata {
    name      = "keycloak"
    namespace = local.ns
    annotations = {
      "cert-manager.io/cluster-issuer" = var.cluster_issuer
    }
  }
  spec {
    ingress_class_name = var.ingress_class
    tls {
      hosts       = [var.host]
      secret_name = "keycloak-tls"
    }
    rule {
      host = var.host
      http {
        # Only what a login needs. Nothing routes /, /admin/, /metrics or /health.
        dynamic "path" {
          for_each = {
            "/realms/"      = kubernetes_service_v1.keycloak.metadata[0].name
            "/resources/"   = kubernetes_service_v1.keycloak.metadata[0].name
            "/.well-known/" = kubernetes_service_v1.keycloak.metadata[0].name
          }
          content {
            path      = path.key
            path_type = "Prefix"
            backend {
              service {
                name = path.value
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
  depends_on             = [aws_route53_record.keycloak]
}

data "aws_route53_zone" "site" {
  count = var.manage_dns_record ? 1 : 0
  name  = var.dns_zone
}

resource "aws_route53_record" "keycloak" {
  count   = var.manage_dns_record ? 1 : 0
  zone_id = data.aws_route53_zone.site[0].zone_id
  name    = var.host
  type    = "A"
  ttl     = 300
  records = [var.node_ip]
}

# A ServiceMonitor is a custom resource, so it is a tiny local chart (like
# cert-manager's ClusterIssuer). The monitoring root's Prometheus picks up
# ServiceMonitors from every namespace.
resource "helm_release" "monitor" {
  count     = var.metrics ? 1 : 0
  name      = "keycloak-monitor"
  namespace = local.ns
  chart     = "${path.module}/charts/keycloak-monitor"

  values = [yamlencode({
    selectorLabels = local.labels
  })]

  depends_on = [kubernetes_service_v1.keycloak]
}
