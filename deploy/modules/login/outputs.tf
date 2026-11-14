output "url" {
  value = "https://${var.host}"
}

output "namespace" {
  value = local.ns
}

output "service_token" {
  description = "Shared service token (also in Secret login/login-service): what game hosts and the coordinator present to the login service."
  value       = random_password.service_token.result
  sensitive   = true
}
