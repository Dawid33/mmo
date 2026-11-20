# Keycloak's old PostgreSQL and retained password. A verified move keeps the
# volume and renames its password Secret while the live Keycloak Secret points
# at the separate pg-management cluster.

locals {
  pg_labels   = { "app.kubernetes.io/name" = "keycloak-postgres" }
  db_host     = var.database == null ? kubernetes_service_v1.postgres[0].metadata[0].name : var.database.host
  db_port     = var.database == null ? 5432 : var.database.port
  db_name     = var.database == null ? "keycloak" : var.database.database
  db_user     = var.database == null ? "keycloak" : var.database.user
  db_password = var.database == null ? random_password.db.result : var.database.password
}

# Changing this does NOT change the password inside an existing volume.
resource "random_password" "db" {
  length  = 32
  special = false
}

resource "kubernetes_secret_v1" "db" {
  metadata {
    name      = "keycloak-db"
    namespace = local.ns
  }
  data = {
    password = local.db_password
  }
}

resource "kubernetes_secret_v1" "db_old" {
  count = var.own_postgres == "stopped" ? 1 : 0
  metadata {
    name      = "keycloak-db-old"
    namespace = local.ns
  }
  data = {
    password = random_password.db.result
  }
}

resource "kubernetes_service_v1" "postgres" {
  count = var.own_postgres == "absent" ? 0 : 1
  metadata {
    name      = "keycloak-postgres"
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
  count = var.own_postgres == "absent" ? 0 : 1
  metadata {
    name      = "keycloak-postgres"
    namespace = local.ns
    labels    = local.pg_labels
  }
  spec {
    service_name = kubernetes_service_v1.postgres[0].metadata[0].name
    replicas     = var.own_postgres == "running" ? 1 : 0
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
            value = "keycloak"
          }
          env {
            name  = "POSTGRES_USER"
            value = "keycloak"
          }
          env {
            name = "POSTGRES_PASSWORD"
            value_from {
              secret_key_ref {
                name = var.own_postgres == "running" ? kubernetes_secret_v1.db.metadata[0].name : kubernetes_secret_v1.db_old[0].metadata[0].name
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
              command = ["pg_isready", "-U", "keycloak", "-d", "keycloak"]
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
  # local-path binds on first consumer; the Deployment's own rollout is the wait.
  wait_for_rollout = var.own_postgres == "running"
}
