output "tenants" {
  description = "Per tenant: its access key and the names of its buckets."
  sensitive   = true
  value = { for tenant, t in var.tenants : tenant => {
    access_key = garage_key.tenant[tenant].id
    secret_key = garage_key.tenant[tenant].secret_access_key
    buckets    = keys(t.buckets)
  } }
}

output "buckets" {
  description = "Every bucket and the tenant it belongs to."
  value       = { for name, b in local.buckets : name => b.tenant }
}
