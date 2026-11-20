output "issuer_name" {
  value = var.issuer_name
}

output "vpn_domain" {
  value = var.vpn_domain
}

output "hosts" {
  description = "Each VPN-only name's host (https where the Service serves TLS on 443, e.g. keycloak; plain http for grafana)."
  value       = { for name, _ in var.names : name => "${name}.${var.vpn_domain}" }
}
