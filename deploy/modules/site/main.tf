# The website (website/site) at https://<host>: static files served by stock
# nginx, shipped as a ConfigMap (a few small text files; no image to build).
# <redirect_hosts> (www) redirect to <host>. The login itself is Keycloak's
# (keycloak + keycloak-realm roots); the page only needs its issuer.

locals {
  ns     = kubernetes_namespace_v1.site.metadata[0].name
  labels = { "app.kubernetes.io/name" = "site" }

  files = { for path in fileset(var.site_dir, "**") : path => file("${var.site_dir}/${path}") }

  # scheme://host[:port] of the issuer, for the CSP's connect-src.
  connect_origin = regex("^[a-z]+://[^/]+", var.issuer)

  nginx_conf = templatefile("${path.module}/nginx.conf.tftpl", {
    host           = var.host
    redirect_hosts = join(" ", var.redirect_hosts)
    connect_origin = local.connect_origin
  })
}

resource "kubernetes_namespace_v1" "site" {
  metadata {
    name = var.namespace
  }
}

# Keys may only hold [-._a-zA-Z0-9], so a path's `/` becomes `__`; the volume's
# `items` restore the real paths under the mount.
resource "kubernetes_config_map_v1" "files" {
  metadata {
    name      = "site-files"
    namespace = local.ns
    labels    = local.labels
  }
  data = { for path, content in local.files : replace(path, "/", "__") => content }

  lifecycle {
    # A ConfigMap is capped at 1 MiB; stay clear of it.
    precondition {
      condition     = sum([for content in values(local.files) : length(content)]) < 900000
      error_message = "The site is over 900000 bytes of content; a ConfigMap holds 1 MiB."
    }
    precondition {
      condition     = strcontains(file("${var.site_dir}/js/config.js"), var.issuer)
      error_message = "website/site/js/config.js must name the realm's issuer"
    }
  }
}

resource "kubernetes_config_map_v1" "nginx" {
  metadata {
    name      = "site-nginx"
    namespace = local.ns
    labels    = local.labels
  }
  data = { "default.conf" = local.nginx_conf }
}

resource "kubernetes_deployment_v1" "site" {
  metadata {
    name      = "site"
    namespace = local.ns
    labels    = local.labels
  }
  spec {
    replicas = 1
    strategy {
      type = "RollingUpdate"
    }
    selector {
      match_labels = local.labels
    }
    template {
      metadata {
        labels = local.labels
        annotations = {
          # Changed files or config roll the pod (mounted ConfigMaps update lazily).
          "checksum/site" = sha256(jsonencode([local.files, local.nginx_conf]))
        }
      }
      spec {
        automount_service_account_token = false
        enable_service_links            = false
        container {
          name  = "nginx"
          image = var.image
          port {
            name           = "http"
            container_port = 8080
          }
          volume_mount {
            name       = "files"
            mount_path = "/site"
            read_only  = true
          }
          volume_mount {
            name       = "nginx"
            mount_path = "/etc/nginx/conf.d/default.conf"
            sub_path   = "default.conf"
            read_only  = true
          }
          resources {
            requests = { cpu = "10m", memory = "16Mi" }
            limits   = { memory = "64Mi" }
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
            initial_delay_seconds = 5
            period_seconds        = 30
          }
        }
        volume {
          name = "files"
          config_map {
            name = kubernetes_config_map_v1.files.metadata[0].name
            dynamic "items" {
              for_each = keys(local.files)
              content {
                key  = replace(items.value, "/", "__")
                path = items.value
              }
            }
          }
        }
        volume {
          name = "nginx"
          config_map {
            name = kubernetes_config_map_v1.nginx.metadata[0].name
          }
        }
      }
    }
  }
}

resource "kubernetes_service_v1" "site" {
  metadata {
    name      = "site"
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

resource "kubernetes_ingress_v1" "site" {
  metadata {
    name      = "site"
    namespace = local.ns
    annotations = {
      "cert-manager.io/cluster-issuer" = var.cluster_issuer
    }
  }
  spec {
    ingress_class_name = var.ingress_class
    tls {
      hosts       = concat([var.host], var.redirect_hosts)
      secret_name = "site-tls"
    }
    dynamic "rule" {
      for_each = concat([var.host], var.redirect_hosts)
      content {
        host = rule.value
        http {
          path {
            path      = "/"
            path_type = "Prefix"
            backend {
              service {
                name = kubernetes_service_v1.site.metadata[0].name
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
}
