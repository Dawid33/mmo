output "grafana_url" {
  value = "https://${var.grafana_host}"
}

output "grafana_admin_password" {
  description = "Grafana `admin` password (also in Secret monitoring/grafana-admin)."
  value       = random_password.grafana_admin.result
  sensitive   = true
}

output "buckets" {
  value = local.buckets
}

output "otlp_endpoints" {
  description = "In-cluster OTLP ingest (Alloy) for the game's traces."
  value = {
    grpc = "alloy.${local.ns}.svc:4317"
    http = "http://alloy.${local.ns}.svc:4318"
  }
}

output "pyroscope_push_url" {
  value = "http://pyroscope.${local.ns}.svc:4040"
}
