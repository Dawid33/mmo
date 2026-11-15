# The twin's PostgreSQL: the CloudNativePG cluster `pg` in namespace `postgres`
# with one world, `twin`, and no backup. Needs ../platform (the operator's CRDs).
module "postgres" {
  source        = "../../../modules/postgres"
  worlds        = ["twin"]
  storage_class = "local-path"
  storage_size  = "2Gi"
  backup        = null
  resources = {
    requests = { cpu = "50m", memory = "128Mi" }
    limits   = { memory = "1Gi" }
  }
}

output "world_urls" {
  description = "World id -> database URL (read by ../world)."
  value       = module.postgres.world_urls
  sensitive   = true
}
