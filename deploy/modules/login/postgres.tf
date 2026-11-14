# The login service's own PostgreSQL: one pod, one volume, one database and one
# role (`login`), no extensions, no custom config. Plain on purpose: the cluster's
# PostgreSQL instances are to be consolidated later. Nothing backs it up yet.

locals {
  pg_labels = { "app.kubernetes.io/name" = "login-postgres" }
}

# Changing this does NOT change the password inside an existing volume.
resource "random_password" "db" {
  length  = 32
  special = false
}

resource "kubernetes_secret_v1" "db" {
  metadata {
    name      = "login-db"
    namespace = local.ns
  }
  data = {
    password = random_password.db.result
  }
}

resource "kubernetes_service_v1" "postgres" {
  metadata {
    name      = "login-postgres"
    namespace = local.ns
    labels    = local.pg_labels
  }
  spec {
    cluster_ip = "None"
    selector   = local.pg_labels
    port {
      name        = "postgres"
      port        = 5432
      target_port = "postgres"
    }
  }
}

resource "kubernetes_stateful_set_v1" "postgres" {
  metadata {
    name      = "login-postgres"
    namespace = local.ns
    labels    = local.pg_labels
  }
  spec {
    service_name = kubernetes_service_v1.postgres.metadata[0].name
    replicas     = 1
    selector {
      match_labels = local.pg_labels
    }
    template {
      metadata {
        labels = local.pg_labels
      }
      spec {
        automount_service_account_token = false
        enable_service_links            = false
        container {
          name  = "postgres"
          image = var.postgres_image
          env {
            name  = "POSTGRES_DB"
            value = "login"
          }
          env {
            name  = "POSTGRES_USER"
            value = "login"
          }
          env {
            name = "POSTGRES_PASSWORD"
            value_from {
              secret_key_ref {
                name = kubernetes_secret_v1.db.metadata[0].name
                key  = "password"
              }
            }
          }
          # A subdirectory: the volume root holds lost+found on some provisioners.
          env {
            name  = "PGDATA"
            value = "/var/lib/postgresql/data/pgdata"
          }
          port {
            name           = "postgres"
            container_port = 5432
          }
          volume_mount {
            name       = "data"
            mount_path = "/var/lib/postgresql/data"
          }
          resources {
            requests = { cpu = "50m", memory = "128Mi" }
            limits   = { memory = var.memory_limits["postgres"] }
          }
          readiness_probe {
            exec {
              command = ["pg_isready", "-U", "login", "-d", "login"]
            }
            period_seconds = 10
          }
        }
      }
    }
    volume_claim_template {
      metadata {
        name = "data"
      }
      spec {
        access_modes       = ["ReadWriteOnce"]
        storage_class_name = var.storage_class
        resources {
          requests = { storage = var.postgres_volume_size }
        }
      }
    }
  }
  # local-path binds on first consumer; the pod becomes Ready once the volume is bound and PostgreSQL answers.
  wait_for_rollout = true
}
