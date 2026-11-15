# The shared PostgreSQL of the orion k3s server (CloudNativePG cluster `pg`, one
# database and role per world) in namespace `postgres`, and its nightly pg_dump
# to Garage. Apply after ../platform (the operator and its CRDs) and
# ../garage-buckets (the world-backups bucket and its key). Order:
#   registry -> platform -> garage-buckets -> postgres -> dns -> world
#   just orion-init postgres && just orion-plan postgres && just orion-apply postgres
# `just orion-garage-admin` is NOT needed here: only garage-buckets' state is read.
variable "state_bucket" {
  description = "The orion state bucket (../backend.s3.tfbackend), to read the garage-buckets root's outputs."
  type        = string
  default     = "mmo-orion-tfstate-029905505680"
}

variable "worlds" {
  description = "World ids: each gets a database and a role."
  type        = list(string)
  default     = ["errantry"]
}

variable "storage_class" {
  type    = string
  default = "local-path"
}

variable "storage_size" {
  description = "k3s local-path volumes CANNOT be expanded, so this is sized for a year of one world, not for today. Growing it later means a dump and restore."
  type        = string
  default     = "20Gi"
}

data "terraform_remote_state" "garage_buckets" {
  backend = "s3"
  config = {
    bucket = var.state_bucket
    region = var.aws_region
    key    = "orion/garage-buckets.tfstate"
  }
}

module "postgres" {
  source        = "../../../modules/postgres"
  worlds        = var.worlds
  storage_class = var.storage_class
  storage_size  = var.storage_size
  backup        = data.terraform_remote_state.garage_buckets.outputs.world_backup
}

output "host" {
  value = module.postgres.host
}

output "world_urls" {
  description = "World id -> database URL (read by ../world)."
  value       = module.postgres.world_urls
  sensitive   = true
}
