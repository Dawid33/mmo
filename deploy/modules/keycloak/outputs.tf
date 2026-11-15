output "url" {
  value = "https://${var.host}"
}

output "namespace" {
  value = local.ns
}

output "admin_url" {
  description = "Where the admin console / API answer: https://<admin_host> over the operator VPN, or with no admin_host http://127.0.0.1:<admin_port> while `just orion-keycloak-admin` runs."
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
