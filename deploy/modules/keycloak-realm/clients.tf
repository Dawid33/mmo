# Zulip (deploy/modules/zulip/oidc.tf): a server-side client with a secret.
resource "keycloak_openid_client" "zulip" {
  realm_id  = keycloak_realm.players.id
  client_id = "zulip"
  name      = "Chat"
  enabled   = true

  access_type                  = "CONFIDENTIAL"
  standard_flow_enabled        = true
  direct_access_grants_enabled = false
  valid_redirect_uris          = ["https://${var.zulip_host}/complete/oidc/"]
  web_origins                  = []
}

# Zulip takes a new user's full name from the `name` claim, which Keycloak
# builds from first + last name: this realm has neither, so send the username.
resource "keycloak_openid_user_property_protocol_mapper" "zulip_name" {
  realm_id  = keycloak_realm.players.id
  client_id = keycloak_openid_client.zulip.id
  name      = "username-as-name"

  user_property       = "username"
  claim_name          = "name"
  add_to_id_token     = true
  add_to_access_token = false
  add_to_userinfo     = true
}

# The game client (native and browser builds): public, no secret. It signs in
# by the device flow: the game shows a code, the player logs in on Keycloak's
# page in a browser, the game polls for the token.
# The standard (redirect) flow is on as well, with PKCE required and redirects
# only back to the browser build's own origins: nothing uses it yet, but
# Keycloak refuses web origins (CORS for the browser build's token calls) on a
# client that has no browser flow at all.
resource "keycloak_openid_client" "game" {
  realm_id  = keycloak_realm.players.id
  client_id = "game"
  name      = "Game"
  enabled   = true

  access_type                               = "PUBLIC"
  standard_flow_enabled                     = true
  pkce_code_challenge_method                = "S256"
  direct_access_grants_enabled              = false
  oauth2_device_authorization_grant_enabled = true
  valid_redirect_uris                       = [for o in var.game_web_origins : "${o}/*"]
  web_origins                               = var.game_web_origins
}

# So a `game` access token says who it is for, and a token minted for another
# client (Zulip's) is refused by the game's login service.
resource "keycloak_openid_audience_protocol_mapper" "game_login" {
  realm_id  = keycloak_realm.players.id
  client_id = keycloak_openid_client.game.id
  name      = "login-service-audience"

  included_custom_audience = var.login_audience
  add_to_id_token          = false
  add_to_access_token      = true
}

# The static website (website/site): a public client, no secret, since the page
# is plain files in the browser. It signs in by the standard flow with PKCE
# (S256) only; no password grant. The login returns to callback.html, a logout
# returns to the front page, and the site's own origin is the only web origin
# (CORS for its token call).
resource "keycloak_openid_client" "site" {
  realm_id  = keycloak_realm.players.id
  client_id = "site"
  name      = "Website"
  enabled   = true

  access_type                     = "PUBLIC"
  standard_flow_enabled           = true
  pkce_code_challenge_method      = "S256"
  direct_access_grants_enabled    = false
  root_url                        = var.site_url
  base_url                        = "/"
  valid_redirect_uris             = ["${var.site_url}/callback.html"]
  valid_post_logout_redirect_uris = ["${var.site_url}/"]
  web_origins                     = [var.site_url]
}
