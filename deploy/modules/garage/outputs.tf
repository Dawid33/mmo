output "namespace" {
  value = local.ns
}

output "s3_region" {
  value = var.s3_region
}

output "s3_endpoint" {
  description = "In-cluster S3 API, host:port, plain HTTP (the telemetry stack)."
  value       = "garage.${local.ns}.svc.cluster.local:${local.ports.s3}"
}

output "s3_url" {
  description = "Public S3 API (Zulip; the lifecycle rules of the garage-buckets root)."
  value       = "https://${var.s3_host}"
}

output "website_urls" {
  description = "Public, read-only URL of each website bucket."
  value       = { for b, h in local.website_hosts : b => "https://${h}" }
}

output "admin_url" {
  description = "The admin API by its in-cluster name: reached over the operator VPN (docs/superpowers/plans/2026-11-15-orion-vpn.md). Without the VPN, `just orion-garage-admin` forwards it to http://127.0.0.1:3903."
  value       = "http://garage.${local.ns}.svc.cluster.local:${local.ports.admin}"
}

output "admin_token" {
  description = "Admin API bearer token (also in Secret garage/garage)."
  value       = random_password.admin_token.result
  sensitive   = true
}
