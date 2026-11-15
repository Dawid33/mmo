# Keycloak (identity provider) on the orion k3s server: https://auth.errantry.renownandruin.com.
# Needs ../platform applied first (cert-manager's ClusterIssuer) and ../monitoring
# (the ServiceMonitor CRD). Realms and clients are ../keycloak-realm's.
#   just orion-init keycloak && just orion-plan keycloak && just orion-apply keycloak
#   just orion-keycloak-admin            # admin console at http://127.0.0.1:8180/admin/
#   just orion-keycloak-admin-password   # password of user `admin`
locals {
  # The login theme lives with the website; every file in it goes to the pod.
  theme_dir = abspath("${path.root}/../../../../website/theme/orion")
}

module "keycloak" {
  source = "../../../modules/keycloak"
  themes = { orion = { for f in fileset(local.theme_dir, "**") : f => file("${local.theme_dir}/${f}") } }
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
