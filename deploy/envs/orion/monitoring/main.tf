# Observability stack on the orion k3s server (5.9.7.138). Needs ../platform
# (cert-manager's ClusterIssuer) applied first.
#   just orion-init monitoring && just orion-plan monitoring && just orion-apply monitoring
#   just orion-grafana-password   # Grafana admin password
module "monitoring" {
  source     = "../../../modules/monitoring"
  aws_region = var.aws_region
}

output "grafana_url" {
  value = module.monitoring.grafana_url
}

output "grafana_admin_password" {
  value     = module.monitoring.grafana_admin_password
  sensitive = true
}

output "buckets" {
  value = module.monitoring.buckets
}

output "otlp_endpoints" {
  value = module.monitoring.otlp_endpoints
}

output "pyroscope_push_url" {
  value = module.monitoring.pyroscope_push_url
}
