output "url" {
  value = "https://${var.host}"
}

output "shared_secret" {
  description = "The upload passphrase (also in Secret picoshare/picoshare)."
  value       = local.secret
  sensitive   = true
}
