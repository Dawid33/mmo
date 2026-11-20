# The buckets and access keys in Garage on the orion k3s server, through its
# admin API. Needs ../garage applied, `just orion-garage-admin` running, and the
# aws CLI on this machine (it sets the lifecycle rules).
#   just orion-init garage-buckets && just orion-plan garage-buckets && just orion-apply garage-buckets
# The roots that use a bucket (../monitoring, ../zulip) read their key and
# bucket names from this root's state: apply it before them, and apply them
# again after rotating a key here.
variable "tenants" {
  description = "See modules/garage-buckets/variables.tf. A bucket with `website = true` also needs its hostname: ../garage, var.website_buckets."
  type = map(object({
    buckets = map(object({
      expiry_days = optional(number)
      website     = optional(bool, false)
    }))
  }))
  default = {
    # The telemetry stack (../monitoring). Expiry is a backstop a little past
    # each component's own retention there, never the retention itself.
    telemetry = {
      buckets = {
        thanos    = { expiry_days = 400 }
        loki      = { expiry_days = 35 }
        tempo     = { expiry_days = 17 }
        pyroscope = { expiry_days = 35 }
      }
    }
    # Zulip (../zulip): people's files, so nothing expires. Avatars are
    # world-readable BY DESIGN: browsers load them straight from the bucket.
    zulip = {
      buckets = {
        zulip-uploads = {}
        zulip-avatars = { website = true }
        zulip-exports = {}
      }
    }
    # Nightly pg_dumps of the game worlds (../postgres, CronJob pg-dump). The
    # dumps are the only off-database copy: nothing expires here, the CronJob
    # prunes to the newest N per world itself.
    world = {
      buckets = {
        world-backups = {}
      }
    }
    # Login and Keycloak's logical dumps (../pg-management). This is separate
    # from the region/world backup key and bucket.
    management = {
      buckets = {
        management-backups = {}
      }
    }
  }
}

module "buckets" {
  source    = "../../../modules/garage-buckets"
  tenants   = var.tenants
  s3_url    = data.terraform_remote_state.garage.outputs.s3_url
  s3_region = data.terraform_remote_state.garage.outputs.s3_region
}

output "buckets" {
  value = module.buckets.buckets
}

output "tenants" {
  description = "Per tenant: access_key, secret_key, buckets."
  value       = module.buckets.tenants
  sensitive   = true
}

# What ../postgres' `backup` needs. The endpoint is the IN-CLUSTER Garage S3 API
# (the CronJob runs in the cluster; `http://` because the script passes it to
# `aws --endpoint-url`).
output "world_backup" {
  value = {
    endpoint   = "http://${data.terraform_remote_state.garage.outputs.s3_endpoint}"
    bucket     = "world-backups"
    access_key = module.buckets.tenants["world"].access_key
    secret_key = module.buckets.tenants["world"].secret_key
  }
  sensitive = true
}

output "management_backup" {
  value = {
    endpoint   = "http://${data.terraform_remote_state.garage.outputs.s3_endpoint}"
    bucket     = "management-backups"
    access_key = module.buckets.tenants["management"].access_key
    secret_key = module.buckets.tenants["management"].secret_key
  }
  sensitive = true
}

# Passed on from the garage root, so a root that uses a bucket reads one state.
output "s3_region" {
  value = data.terraform_remote_state.garage.outputs.s3_region
}

output "s3_endpoint" {
  description = "In-cluster S3 API, host:port, plain HTTP."
  value       = data.terraform_remote_state.garage.outputs.s3_endpoint
}

output "s3_url" {
  description = "Public S3 API, HTTPS."
  value       = data.terraform_remote_state.garage.outputs.s3_url
}

output "website_urls" {
  value = data.terraform_remote_state.garage.outputs.website_urls
}
