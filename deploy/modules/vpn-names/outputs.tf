output "issuer_name" {
  value = var.issuer_name
}

output "vpn_domain" {
  value = var.vpn_domain
}

output "urls" {
  description = "Each VPN-only name as an https URL (the Service serves TLS on 443)."
  value       = { for name, _ in var.names : name => "https://${name}.${var.vpn_domain}" }
}
