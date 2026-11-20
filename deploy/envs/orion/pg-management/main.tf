# The management CloudNativePG cluster: login and Keycloak only. Apply after
# platform (CNPG CRDs) and garage-buckets (management-backups), before keycloak
# and login. It never reads or writes the region cluster's postgres state.
variable "state_bucket" {
  description = "The orion state bucket (../backend.s3.tfbackend)."
  type        = string
  default     = "mmo-orion-tfstate-029905505680"
}

variable "storage_class" {
  type    = string
  default = "local-path"
}

variable "storage_size" {
  type    = string
  default = "20Gi"
}

data "terraform_remote_state" "garage_buckets" {
  backend = "s3"
  config = {
    bucket = var.state_bucket
    region = var.aws_region
    key    = "orion/garage-buckets.tfstate"
  }
}

module "pg_management" {
  source        = "../../../modules/pg-management"
  storage_class = var.storage_class
  storage_size  = var.storage_size
  backup        = data.terraform_remote_state.garage_buckets.outputs.management_backup
}

output "databases" {
  value     = module.pg_management.databases
  sensitive = true
}
