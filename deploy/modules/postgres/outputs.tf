output "host" {
  description = "The read-write service of the cluster. Hosts connect here directly (no pooler)."
  value       = local.host
}

output "world_urls" {
  description = "World id -> database URL of that world's role."
  sensitive   = true
  value = {
    for w in var.worlds :
    w => "postgres://world_${w}:${random_password.world[w].result}@${local.host}:5432/world_${w}"
  }
}
