# Nightly logical dump of every world database to the S3-compatible store
# (Garage). Init containers (one per world, the PostgreSQL image) dump into an
# emptyDir as that world's own role; the main container uploads and prunes
# (pg-dump.sh). Any failing step fails the Job; alerting is the monitoring
# root's job. Manual run: kubectl -n <ns> create job --from=cronjob/pg-dump pg-dump-<n>.

locals {
  # Garage signs with its own region name, not an AWS one.
  s3_region = "garage"
}

resource "kubernetes_secret_v1" "backup" {
  count = var.backup == null ? 0 : 1
  metadata {
    name      = "pg-dump-s3"
    namespace = kubernetes_namespace_v1.postgres.metadata[0].name
  }
  data = {
    AWS_ACCESS_KEY_ID     = var.backup.access_key
    AWS_SECRET_ACCESS_KEY = var.backup.secret_key
  }
}

resource "kubernetes_config_map_v1" "backup" {
  count = var.backup == null ? 0 : 1
  metadata {
    name      = "pg-dump"
    namespace = kubernetes_namespace_v1.postgres.metadata[0].name
  }
  data = {
    "pg-dump.sh" = file("${path.module}/pg-dump.sh")
  }
}

resource "kubernetes_cron_job_v1" "backup" {
  count = var.backup == null ? 0 : 1
  metadata {
    name      = "pg-dump"
    namespace = kubernetes_namespace_v1.postgres.metadata[0].name
  }
  spec {
    schedule                      = var.backup_schedule
    concurrency_policy            = "Forbid"
    successful_jobs_history_limit = 1
    failed_jobs_history_limit     = 3
    job_template {
      metadata {}
      spec {
        backoff_limit = 2
        # A hung dump must not block the next night's run (concurrency Forbid).
        active_deadline_seconds = 3600
        template {
          metadata {}
          spec {
            automount_service_account_token = false
            enable_service_links            = false
            restart_policy                  = "OnFailure"

            dynamic "init_container" {
              for_each = var.worlds
              content {
                name    = "dump-${init_container.value}"
                image   = var.image
                command = ["/bin/sh", "/scripts/pg-dump.sh", "dump"]
                env {
                  name  = "WORLD"
                  value = init_container.value
                }
                env {
                  name  = "PGHOST"
                  value = local.host
                }
                env {
                  name  = "PGDATABASE"
                  value = "world_${init_container.value}"
                }
                env {
                  name = "PGUSER"
                  value_from {
                    secret_key_ref {
                      name = kubernetes_secret_v1.world[init_container.value].metadata[0].name
                      key  = "username"
                    }
                  }
                }
                env {
                  name = "PGPASSWORD"
                  value_from {
                    secret_key_ref {
                      name = kubernetes_secret_v1.world[init_container.value].metadata[0].name
                      key  = "password"
                    }
                  }
                }
                env {
                  name  = "DUMP_DIR"
                  value = "/dump"
                }
                env {
                  name  = "KEEP"
                  value = tostring(var.backup_keep)
                }
                volume_mount {
                  name       = "scripts"
                  mount_path = "/scripts"
                  read_only  = true
                }
                volume_mount {
                  name       = "dump"
                  mount_path = "/dump"
                }
                resources {
                  requests = { cpu = "50m", memory = "128Mi" }
                  limits   = { memory = "512Mi" }
                }
              }
            }

            container {
              name    = "upload"
              image   = var.backup_image
              command = ["/bin/sh", "/scripts/pg-dump.sh", "upload"]
              env_from {
                secret_ref {
                  name = kubernetes_secret_v1.backup[0].metadata[0].name
                }
              }
              env {
                name  = "AWS_DEFAULT_REGION"
                value = local.s3_region
              }
              env {
                name  = "S3_ENDPOINT"
                value = var.backup.endpoint
              }
              env {
                name  = "BUCKET"
                value = var.backup.bucket
              }
              env {
                name  = "KEEP"
                value = tostring(var.backup_keep)
              }
              env {
                name  = "DUMP_DIR"
                value = "/dump"
              }
              # Garage does not take the CRC checksums newer AWS CLIs send and expect by
              # default: only when an operation requires one.
              env {
                name  = "AWS_REQUEST_CHECKSUM_CALCULATION"
                value = "when_required"
              }
              env {
                name  = "AWS_RESPONSE_CHECKSUM_VALIDATION"
                value = "when_required"
              }
              volume_mount {
                name       = "scripts"
                mount_path = "/scripts"
                read_only  = true
              }
              volume_mount {
                name       = "dump"
                mount_path = "/dump"
              }
              resources {
                requests = { cpu = "10m", memory = "64Mi" }
                limits   = { memory = "256Mi" }
              }
            }

            volume {
              name = "scripts"
              config_map {
                name = kubernetes_config_map_v1.backup[0].metadata[0].name
              }
            }
            volume {
              name = "dump"
              empty_dir {
                size_limit = var.backup_scratch_size
              }
            }
          }
        }
      }
    }
  }
}
