# Pulling the login image from the private ECR registry (the registry root).
# The node has no AWS role, and an ECR pull token lives 12 hours, so:
# - Secret login-ecr (the pods' imagePullSecret) is made at apply time from a
#   token of the pull-only user (provider aws.ecr), so the very first pod can
#   pull without waiting for the CronJob; afterwards its data is ignored here.
# - CronJob ecr-refresh rewrites it every 6 hours (ecr-refresh.sh) with the pull
#   user's key (Secret ecr-pull-aws), through a ServiceAccount that may only
#   get / patch / update that one Secret.
# A missed refresh only matters to a pull: on this single node the image is
# cached and pods restart without one (image_pull_policy IfNotPresent).
# Manual run: `just orion-kube -n login create job --from=cronjob/ecr-refresh ecr-refresh-<n>`.
#
# All of it only with var.ecr set. var.ecr = null (the k3d twin: the image was
# imported into the node) makes none of it and the pods list no imagePullSecrets.
# The objects gained `count` after they were first applied on orion: the moved
# blocks at the end keep their state addresses (a plan shows no change).

locals {
  registry_host = split("/", var.image_repository)[0]
  ecr           = var.ecr == null ? 0 : 1
}

# Read at every plan, and lands in the state like the other secrets here.
data "aws_ecr_authorization_token" "pull" {
  count    = local.ecr
  provider = aws.ecr
}

resource "kubernetes_secret_v1" "ecr_pull" {
  count = local.ecr
  metadata {
    name      = "login-ecr"
    namespace = local.ns
  }
  type = "kubernetes.io/dockerconfigjson"
  data = {
    ".dockerconfigjson" = jsonencode({
      auths = {
        (local.registry_host) = {
          username = "AWS"
          password = data.aws_ecr_authorization_token.pull[0].password
          auth     = data.aws_ecr_authorization_token.pull[0].authorization_token
        }
      }
    })
  }
  # The CronJob owns the token from here on; an apply must not put back the
  # (by then expired) apply-time one.
  lifecycle {
    ignore_changes = [data]
  }
}

resource "kubernetes_secret_v1" "ecr_pull_aws" {
  count = local.ecr
  metadata {
    name      = "ecr-pull-aws"
    namespace = local.ns
  }
  data = {
    AWS_ACCESS_KEY_ID     = var.ecr.access_key_id
    AWS_SECRET_ACCESS_KEY = var.ecr.secret_access_key
  }
}

resource "kubernetes_service_account_v1" "ecr_refresh" {
  count = local.ecr
  metadata {
    name      = "ecr-refresh"
    namespace = local.ns
  }
  # Mounted only into the CronJob's pod (below).
  automount_service_account_token = false
}

resource "kubernetes_role_v1" "ecr_refresh" {
  count = local.ecr
  metadata {
    name      = "ecr-refresh"
    namespace = local.ns
  }
  rule {
    api_groups     = [""]
    resources      = ["secrets"]
    resource_names = [kubernetes_secret_v1.ecr_pull[0].metadata[0].name]
    verbs          = ["get", "patch", "update"]
  }
}

resource "kubernetes_role_binding_v1" "ecr_refresh" {
  count = local.ecr
  metadata {
    name      = "ecr-refresh"
    namespace = local.ns
  }
  role_ref {
    api_group = "rbac.authorization.k8s.io"
    kind      = "Role"
    name      = kubernetes_role_v1.ecr_refresh[0].metadata[0].name
  }
  subject {
    kind      = "ServiceAccount"
    name      = kubernetes_service_account_v1.ecr_refresh[0].metadata[0].name
    namespace = local.ns
  }
}

resource "kubernetes_cron_job_v1" "ecr_refresh" {
  count = local.ecr
  metadata {
    name      = "ecr-refresh"
    namespace = local.ns
  }
  spec {
    schedule                      = var.refresh_schedule
    concurrency_policy            = "Forbid"
    successful_jobs_history_limit = 1
    failed_jobs_history_limit     = 2
    job_template {
      metadata {}
      spec {
        backoff_limit = 3
        template {
          metadata {}
          spec {
            service_account_name = kubernetes_service_account_v1.ecr_refresh[0].metadata[0].name
            # This pod alone talks to the API server.
            automount_service_account_token = true
            enable_service_links            = false
            restart_policy                  = "OnFailure"
            container {
              name    = "refresh"
              image   = var.refresh_image
              command = ["/bin/sh", "-c", file("${path.module}/ecr-refresh.sh")]
              env_from {
                secret_ref {
                  name = kubernetes_secret_v1.ecr_pull_aws[0].metadata[0].name
                }
              }
              env {
                name  = "AWS_DEFAULT_REGION"
                value = var.ecr.region
              }
              env {
                name  = "REGISTRY_HOST"
                value = local.registry_host
              }
              env {
                name  = "PULL_SECRET"
                value = kubernetes_secret_v1.ecr_pull[0].metadata[0].name
              }
              # The token's files live in memory only.
              env {
                name  = "TMPDIR"
                value = "/work"
              }
              volume_mount {
                name       = "work"
                mount_path = "/work"
              }
              resources {
                requests = { cpu = "10m", memory = "64Mi" }
                limits   = { memory = "256Mi" }
              }
            }
            volume {
              name = "work"
              empty_dir {
                medium     = "Memory"
                size_limit = "1Mi"
              }
            }
          }
        }
      }
    }
  }
}

moved {
  from = kubernetes_secret_v1.ecr_pull
  to   = kubernetes_secret_v1.ecr_pull[0]
}

moved {
  from = kubernetes_secret_v1.ecr_pull_aws
  to   = kubernetes_secret_v1.ecr_pull_aws[0]
}

moved {
  from = kubernetes_service_account_v1.ecr_refresh
  to   = kubernetes_service_account_v1.ecr_refresh[0]
}

moved {
  from = kubernetes_role_v1.ecr_refresh
  to   = kubernetes_role_v1.ecr_refresh[0]
}

moved {
  from = kubernetes_role_binding_v1.ecr_refresh
  to   = kubernetes_role_binding_v1.ecr_refresh[0]
}

moved {
  from = kubernetes_cron_job_v1.ecr_refresh
  to   = kubernetes_cron_job_v1.ecr_refresh[0]
}
