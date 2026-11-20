# The `staff` realm: the people who run the server, kept apart from player
# accounts. Nobody can register here: an administrator creates each account in
# the admin console (realm `staff` -> Users -> Add user, then Credentials ->
# set a temporary password). Every account must set up two-factor login
# (an authenticator app) at its first login.
# What logs in through it: Grafana (deploy/modules/monitoring, grafana_oidc).

resource "keycloak_realm" "staff" {
  realm        = var.staff_realm
  enabled      = true
  display_name = var.staff_display_name

  registration_allowed     = false
  login_with_email_allowed = true
  duplicate_emails_allowed = false
  verify_email             = true
  reset_password_allowed   = true
  remember_me              = false
  edit_username_allowed    = false
  ssl_required             = "external"

  security_defenses {
    brute_force_detection {
      max_login_failures = 5
    }
  }

  smtp_server {
    host              = module.smtp.host
    port              = module.smtp.port
    starttls          = true
    from              = module.smtp.sender
    from_display_name = var.staff_display_name
    auth {
      username = module.smtp.username
      password = module.smtp.password
    }
  }
}

# Username and email only, like the players realm.
resource "keycloak_realm_user_profile" "staff" {
  realm_id = keycloak_realm.staff.id

  attribute {
    name               = "username"
    display_name       = "$${username}"
    required_for_roles = ["user"]
    permissions {
      view = ["admin", "user"]
      edit = ["admin", "user"]
    }
    validator {
      name   = "length"
      config = { min = "3", max = "24" }
    }
    validator {
      name = "username-prohibited-characters"
    }
  }

  attribute {
    name               = "email"
    display_name       = "$${email}"
    required_for_roles = ["user"]
    permissions {
      view = ["admin", "user"]
      edit = ["admin", "user"]
    }
    validator {
      name = "email"
    }
  }
}

# Two-factor for everyone: a new account has to configure an authenticator app
# before its first login completes.
resource "keycloak_required_action" "staff_totp" {
  realm_id       = keycloak_realm.staff.realm
  alias          = "CONFIGURE_TOTP"
  name           = "Configure OTP"
  enabled        = true
  default_action = true
}

# Grafana. Being staff is not enough: Grafana refuses anyone without one of
# these client roles (role_attribute_strict). Give a role in the admin console:
# realm `staff` -> Users -> <user> -> Role mapping -> Assign role -> client
# roles -> grafana admin / editor / viewer.
resource "keycloak_openid_client" "grafana" {
  realm_id  = keycloak_realm.staff.id
  client_id = "grafana"
  name      = "Dashboards"
  enabled   = true

  access_type                  = "CONFIDENTIAL"
  standard_flow_enabled        = true
  direct_access_grants_enabled = false
  valid_redirect_uris          = ["${var.grafana_url}/login/generic_oauth"]
  web_origins                  = []
}

resource "keycloak_role" "grafana" {
  for_each    = toset(["admin", "editor", "viewer"])
  realm_id    = keycloak_realm.staff.id
  client_id   = keycloak_openid_client.grafana.id
  name        = each.key
  description = "Grafana ${each.key}"
}

# The person's grafana roles as a list claim; Grafana maps it to its own roles.
resource "keycloak_openid_user_client_role_protocol_mapper" "grafana_roles" {
  realm_id  = keycloak_realm.staff.id
  client_id = keycloak_openid_client.grafana.id
  name      = "grafana-roles"

  claim_name                  = "grafana_roles"
  client_id_for_role_mappings = keycloak_openid_client.grafana.client_id
  multivalued                 = true
  add_to_id_token             = true
  add_to_access_token         = true
  add_to_userinfo             = true
}
