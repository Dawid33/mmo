# Keycloak (identity provider) on the orion k3s server: https://auth.errantry.renownandruin.com.
# Needs ../platform applied first (cert-manager's ClusterIssuer) and ../monitoring
# (the ServiceMonitor CRD). Realms and clients are ../keycloak-realm's.
#   just orion-init keycloak && just orion-plan keycloak && just orion-apply keycloak
# Admin console and API: https://keycloak.vpn.errantry.renownandruin.com/admin/ over the operator
# VPN (needs ../vpn-names applied first; docs/superpowers/plans/2026-11-15-orion-vpn.md).
#   just orion-keycloak-admin-password   # password of user `admin`
locals {
  # The login theme lives with the website; every file in it goes to the pod.
  theme_dir = abspath("${path.root}/../../../../website/theme/orion")
}

module "keycloak" {
  source = "../../../modules/keycloak"
  themes = { orion = { for f in fileset(local.theme_dir, "**") : f => file("${local.theme_dir}/${f}") } }
  # A vpn-names name (that root's `names`): resolves to this Service only over the VPN.
  admin_host = "keycloak.vpn.errantry.renownandruin.com"
}

output "url" {
  value = module.keycloak.url
}

output "namespace" {
  value = module.keycloak.namespace
}

output "admin_url" {
  value = module.keycloak.admin_url
}

output "admin_username" {
  value = module.keycloak.admin_username
}

output "admin_password" {
  value     = module.keycloak.admin_password
  sensitive = true
}
