# Keycloak (identity provider) on the orion k3s server: https://auth.errantry.renownandruin.com.
# Needs ../platform applied first (cert-manager's ClusterIssuer) and ../monitoring
# (the ServiceMonitor CRD). Realms and clients are ../keycloak-realm's.
#   just orion-init keycloak && just orion-plan keycloak && just orion-apply keycloak
# Admin console and API: https://keycloak.vpn.errantry.renownandruin.com/admin/ over the operator
# VPN (needs ../vpn-names applied first; docs/superpowers/plans/2026-11-15-orion-vpn.md).
#   just orion-keycloak-admin-password   # password of user `admin`
variable "state_bucket" {
  type    = string
  default = "mmo-orion-tfstate-029905505680"
}

variable "use_management_db" {
  description = "Keycloak's data lives in pg-management. Keep true: switching it off would point a normal deploy at the retired StatefulSet."
  type        = bool
  default     = true
}

variable "own_postgres" {
  description = "The retired Keycloak PostgreSQL is absent after the completed move. stopped is a pre-cleanup rehearsal state; running is a rollback-only override."
  type        = string
  default     = "absent"
}

locals {
  # The login theme lives with the website; every file in it goes to the pod.
  theme_dir = abspath("${path.root}/../../../../website/theme/orion")
}

data "terraform_remote_state" "pg_management" {
  backend = "s3"
  config = {
    bucket = var.state_bucket
    region = var.aws_region
    key    = "orion/pg-management.tfstate"
  }
}

module "keycloak" {
  source = "../../../modules/keycloak"
  themes = { orion = { for f in fileset(local.theme_dir, "**") : f => file("${local.theme_dir}/${f}") } }
  # A vpn-names name (that root's `names`): resolves to this Service only over the VPN.
  admin_host   = "keycloak.vpn.errantry.renownandruin.com"
  database     = var.use_management_db ? data.terraform_remote_state.pg_management.outputs.databases["keycloak"] : null
  own_postgres = var.own_postgres
}

output "url" {
  value = module.keycloak.url
}

output "namespace" {
  value = module.keycloak.namespace
}

output "admin_url" {
  value = module.keycloak.admin_url
}

output "admin_username" {
  value = module.keycloak.admin_username
}

output "admin_password" {
  value     = module.keycloak.admin_password
  sensitive = true
}
