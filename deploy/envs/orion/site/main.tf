# The website at https://factoriosurvivalworld.com (www redirects to it): static
# files from website/site behind stock nginx. Reads the realm's issuer from the
# keycloak-realm root, so apply keycloak -> keycloak-realm first.
#   just orion-init site && just orion-plan site && just orion-apply site
module "site" {
  source   = "../../../modules/site"
  site_dir = abspath("${path.root}/../../../../website/site")
  issuer   = data.terraform_remote_state.realm.outputs.issuer
}

output "url" {
  value = module.site.url
}
