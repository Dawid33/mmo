# Observability stack on the orion k3s server (5.9.7.138). Needs ../platform
# (cert-manager's ClusterIssuer) and ../garage + ../garage-buckets (the object
# store its data lives in, and the `telemetry` key) applied first.
#   just orion-init monitoring && just orion-plan monitoring && just orion-apply monitoring
#   just orion-grafana-password   # Grafana admin password
variable "zulip_alert_webhook_url" {
  description = "Zulip bot URL every alert is posted to (see modules/monitoring/variables.tf). Null: no alert notifications."
  type        = string
  default     = null
  sensitive   = true
  validation {
    condition     = var.zulip_alert_webhook_url == null || !strcontains(coalesce(var.zulip_alert_webhook_url, "REPLACE_"), "REPLACE_")
    error_message = "zulip_alert_webhook_url still holds a REPLACE_ placeholder: put the bot's API key in terraform.tfvars."
  }
}

variable "keycloak_login" {
  description = "Log in to Grafana through Keycloak's `staff` realm (client `grafana`; only staff given a grafana role there get in). Reads the issuer and client secret from the ../keycloak-realm root's state, so that root must be applied first. False: the local `admin` login only."
  type        = bool
  default     = false
}

variable "grafana_password_login" {
  description = "Keep Grafana's username / password form next to Keycloak (modules/monitoring/variables.tf)."
  type        = bool
  default     = true
}

variable "state_bucket" {
  description = "The orion state bucket (../backend.s3.tfbackend), to read the garage-buckets and keycloak-realm roots' outputs."
  type        = string
  default     = "mmo-orion-tfstate-029905505680"
}

data "terraform_remote_state" "keycloak_realm" {
  count   = var.keycloak_login ? 1 : 0
  backend = "s3"
  config = {
    bucket = var.state_bucket
    region = var.aws_region
    key    = "orion/keycloak-realm.tfstate"
  }
}

# Garage's address, the telemetry buckets and their access key.
data "terraform_remote_state" "garage_buckets" {
  backend = "s3"
  config = {
    bucket = var.state_bucket
    region = var.aws_region
    key    = "orion/garage-buckets.tfstate"
  }
}

locals {
  garage = data.terraform_remote_state.garage_buckets.outputs
}

module "monitoring" {
  source = "../../../modules/monitoring"
  object_store = {
    endpoint = local.garage.s3_endpoint
    region   = local.garage.s3_region
    # Every bucket of the `telemetry` tenant, by its own name: thanos, loki, ...
    buckets = { for name, tenant in local.garage.buckets : name => name if tenant == "telemetry" }
  }
  object_store_key = {
    access_key = local.garage.tenants["telemetry"].access_key
    secret_key = local.garage.tenants["telemetry"].secret_key
  }
  zulip_alert_webhook_url = var.zulip_alert_webhook_url
  grafana_password_login  = var.grafana_password_login
  grafana_oidc = var.keycloak_login ? {
    issuer       = data.terraform_remote_state.keycloak_realm[0].outputs.staff_issuer
    client_id    = data.terraform_remote_state.keycloak_realm[0].outputs.grafana_client_id
    display_name = "Staff account"
  } : null
  grafana_oidc_client_secret = var.keycloak_login ? data.terraform_remote_state.keycloak_realm[0].outputs.grafana_client_secret : null
}

output "grafana_url" {
  value = module.monitoring.grafana_url
}

output "grafana_admin_password" {
  value     = module.monitoring.grafana_admin_password
  sensitive = true
}

output "buckets" {
  value = module.monitoring.buckets
}

output "otlp_endpoints" {
  value = module.monitoring.otlp_endpoints
}

output "pyroscope_push_url" {
  value = module.monitoring.pyroscope_push_url
}
