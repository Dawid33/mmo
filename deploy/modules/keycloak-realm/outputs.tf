output "issuer" {
  description = "OpenID Connect issuer of the realm (discovery: <issuer>/.well-known/openid-configuration)."
  value       = "${var.public_url}/realms/${keycloak_realm.players.realm}"
}

output "zulip_client_id" {
  value = keycloak_openid_client.zulip.client_id
}

output "zulip_client_secret" {
  value     = keycloak_openid_client.zulip.client_secret
  sensitive = true
}

output "game_client_id" {
  value = keycloak_openid_client.game.client_id
}

output "site_client_id" {
  value = keycloak_openid_client.site.client_id
}

output "login_audience" {
  value = var.login_audience
}

output "staff_issuer" {
  description = "OpenID Connect issuer of the staff realm (Grafana logs in here)."
  value       = "${var.public_url}/realms/${keycloak_realm.staff.realm}"
}

output "grafana_client_id" {
  value = keycloak_openid_client.grafana.client_id
}

output "grafana_client_secret" {
  value     = keycloak_openid_client.grafana.client_secret
  sensitive = true
}
