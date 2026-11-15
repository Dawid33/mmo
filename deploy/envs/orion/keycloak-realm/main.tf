# Keycloak's realms on orion. `players`: sign-up rules, account mail (SES), and
# the clients that log in through it (`zulip`, `game`, `site`). `staff`: the people who
# run the server (no self-registration, two-factor), for Grafana. Needs ../keycloak
# applied, `just orion-keycloak-admin` RUNNING in another terminal, and the
# service-account secret in terraform.tfvars (see terraform.tfvars.example).
#   just orion-init keycloak-realm && just orion-plan keycloak-realm && just orion-apply keycloak-realm
# ../zulip reads this root's outputs from its state.
module "realm" {
  source = "../../../modules/keycloak-realm"
  providers = {
    aws     = aws
    aws.ses = aws.ses
  }
  public_url = data.terraform_remote_state.keycloak.outputs.url
}

output "issuer" {
  value = module.realm.issuer
}

output "zulip_client_id" {
  value = module.realm.zulip_client_id
}

output "zulip_client_secret" {
  value     = module.realm.zulip_client_secret
  sensitive = true
}

output "game_client_id" {
  value = module.realm.game_client_id
}

output "site_client_id" {
  value = module.realm.site_client_id
}

output "login_audience" {
  value = module.realm.login_audience
}

output "staff_issuer" {
  value = module.realm.staff_issuer
}

output "grafana_client_id" {
  value = module.realm.grafana_client_id
}

output "grafana_client_secret" {
  value     = module.realm.grafana_client_secret
  sensitive = true
}
