# PicoShare (github.com/mtlynch/picoshare): minimal file sharing at https://<host>.
# One pod, one SQLite file (/data/store.db) on a PV — never more than one
# replica, and `Recreate` so two pods never open the store together. Uploads
# need the shared secret; download links are public to whoever has them.
# Long uploads also need the ingress not to cut them: modules/traefik-config.

locals {
  ns     = kubernetes_namespace_v1.picoshare.metadata[0].name
  labels = { "app.kubernetes.io/name" = "picoshare" }
  port   = 4001
  secret = coalesce(var.shared_secret, random_password.shared_secret.result)
}

resource "kubernetes_namespace_v1" "picoshare" {
  metadata {
    name = var.namespace
  }
}

resource "random_password" "shared_secret" {
  length  = 32
  special = false
}

resource "kubernetes_secret_v1" "picoshare" {
  metadata {
    name      = "picoshare"
    namespace = local.ns
  }
  data = {
    PS_SHARED_SECRET = local.secret
  }
}

resource "kubernetes_persistent_volume_claim_v1" "data" {
  metadata {
    name      = "picoshare-data"
    namespace = local.ns
  }
  spec {
    access_modes       = ["ReadWriteOnce"]
    storage_class_name = var.storage_class
    resources {
      requests = { storage = var.volume_size }
    }
  }
  # local-path binds on first consumer (the Deployment below).
  wait_until_bound = false
}

resource "kubernetes_deployment_v1" "picoshare" {
  metadata {
    name      = "picoshare"
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
          # A changed secret restarts the pod (env is read once).
          "checksum/secret" = sha256(local.secret)
        }
      }
      spec {
        # Nothing here talks to the Kubernetes API.
        automount_service_account_token = false
        enable_service_links            = false
        container {
          name  = "picoshare"
          image = var.image
          env {
            name  = "PORT"
            value = tostring(local.port)
          }
          env_from {
            secret_ref {
              name = kubernetes_secret_v1.picoshare.metadata[0].name
            }
          }
          port {
            name           = "http"
            container_port = local.port
          }
          volume_mount {
            name       = "data"
            mount_path = "/data"
          }
          resources {
            requests = { cpu = "10m", memory = "64Mi" }
            limits   = { memory = var.memory_limit }
          }
          readiness_probe {
            http_get {
              path = "/"
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
        volume {
          name = "data"
          persistent_volume_claim {
            claim_name = kubernetes_persistent_volume_claim_v1.data.metadata[0].name
          }
        }
      }
    }
  }
}

resource "kubernetes_service_v1" "picoshare" {
  metadata {
    name      = "picoshare"
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

resource "kubernetes_ingress_v1" "picoshare" {
  metadata {
    name      = "picoshare"
    namespace = local.ns
    annotations = {
      "cert-manager.io/cluster-issuer" = var.cluster_issuer
    }
  }
  spec {
    ingress_class_name = var.ingress_class
    tls {
      hosts       = [var.host]
      secret_name = "picoshare-tls"
    }
    rule {
      host = var.host
      http {
        path {
          path      = "/"
          path_type = "Prefix"
          backend {
            service {
              name = kubernetes_service_v1.picoshare.metadata[0].name
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
