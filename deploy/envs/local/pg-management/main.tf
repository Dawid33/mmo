# The twin's management CloudNativePG cluster. It has no Garage backup and is
# intentionally disposable with target/twin/.
module "pg_management" {
  source        = "../../../modules/pg-management"
  storage_class = "local-path"
  storage_size  = "2Gi"
  backup        = null
  resources = {
    requests = { cpu = "50m", memory = "128Mi" }
    limits   = { memory = "1Gi" }
  }
}

output "databases" {
  value     = module.pg_management.databases
  sensitive = true
}
