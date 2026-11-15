output "url" {
  value = "https://${var.host}"
}

output "namespace" {
  value = local.ns
}

output "admin_url" {
  description = "Where the admin console / API answer while `just orion-keycloak-admin` runs."
  value       = local.admin_url
}

output "admin_username" {
  value = "admin"
}

output "admin_password" {
  description = "Password of the `master` realm's admin (also in Secret keycloak/keycloak-admin)."
  value       = random_password.admin.result
  sensitive   = true
}
