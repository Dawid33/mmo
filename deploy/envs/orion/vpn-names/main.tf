# Names that answer only over the operator VPN (docs/superpowers/plans/2026-11-15-orion-vpn.md):
# https://<name>.vpn.errantry.renownandruin.com -> a Service's ClusterIP, with a Let's Encrypt
# certificate from the DNS-01 ClusterIssuer `letsencrypt-dns`. Needs ../platform (cert-manager).
# Apply before a root that uses a name (keycloak: its admin console; monitoring: Grafana).
#   just orion-init vpn-names && just orion-plan vpn-names && just orion-apply vpn-names
module "vpn_names" {
  source = "../../../modules/vpn-names"
}

output "issuer_name" {
  value = module.vpn_names.issuer_name
}

output "vpn_domain" {
  value = module.vpn_names.vpn_domain
}

output "hosts" {
  value = module.vpn_names.hosts
}
