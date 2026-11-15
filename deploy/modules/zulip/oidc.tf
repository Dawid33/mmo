# Login through an OpenID Connect identity provider (var.oidc; on orion:
# Keycloak's `players` realm, deploy/modules/keycloak-realm). Zulip then shows a
# "Log in with <display_name>" button, creates the Zulip account on a person's
# first login (auto_signup) and takes their full name from the provider, so
# nobody types a password into Zulip. Accounts are matched BY EMAIL (matching by
# the provider's user id needs Zulip 13), which is only safe because the
# provider verifies email addresses before it lets anyone log in.

locals {
  # docker-zulip writes a value shaped `{...}` into settings.py verbatim, so
  # this is Python: True / None, and get_secret() reads zulip-secrets.conf
  # (SECRETS_social_auth_oidc_secret, main.tf). The key "keycloak" is only a
  # label inside Zulip.
  oidc_idps = var.oidc == null ? null : join("", [
    "{\"keycloak\": {",
    "\"oidc_url\": ${jsonencode(var.oidc.issuer)}, ",
    "\"display_name\": ${jsonencode(var.oidc.display_name)}, ",
    "\"display_icon\": None, ",
    "\"client_id\": ${jsonencode(var.oidc.client_id)}, ",
    "\"secret\": get_secret(\"social_auth_oidc_secret\"), ",
    "\"auto_signup\": True",
    "}}",
  ])

  oidc_env = var.oidc == null ? {} : {
    SETTING_SOCIAL_AUTH_OIDC_ENABLED_IDPS = local.oidc_idps
    # Trust the provider's name: a new user skips Zulip's registration form.
    SETTING_SOCIAL_AUTH_OIDC_FULL_NAME_VALIDATED = "True"
    # No "Sign up" button on the login page or in the page header: accounts are
    # made in the identity provider, and Zulip makes its own side on the first
    # login (auto_signup above, which this does not touch — the setting only
    # hides the links; /register itself still answers, with the same single
    # "Log in with ..." button).
    SETTING_REGISTER_LINK_DISABLED = "True"
    # Replaces Zulip's AUTHENTICATION_BACKENDS (default: EmailAuthBackend only).
    ZULIP_AUTH_BACKENDS = var.password_login ? "EmailAuthBackend,GenericOpenIdConnectBackend" : "GenericOpenIdConnectBackend"
  }
}
