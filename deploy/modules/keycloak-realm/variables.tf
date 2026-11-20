variable "realm" {
  description = "Realm name: part of every token's issuer (<keycloak url>/realms/<realm>), so fixed once anything trusts it."
  type        = string
  default     = "players"
}

variable "staff_realm" {
  description = "Realm of the people who run the server (no self-registration, two-factor required). Part of its tokens' issuer, so fixed once anything trusts it."
  type        = string
  default     = "staff"
}

variable "staff_display_name" {
  type    = string
  default = "Staff account"
}

variable "public_url" {
  description = "Keycloak's public URL (the keycloak root's `url`)."
  type        = string
  default     = "https://auth.errantry.renownandruin.com"
}

variable "display_name" {
  description = "What the login pages and mails call the account."
  type        = string
  default     = "Errantry"
}

variable "login_theme" {
  description = "Login theme of the `players` realm (login, register, reset password, errors, device code). `orion` is the site's look, mounted into the Keycloak pod by ../keycloak from website/theme; the `staff` realm keeps Keycloak's default."
  type        = string
  default     = "orion"
}

# --- Mail --------------------------------------------------------------------

variable "mail_domain" {
  description = "Existing SES domain identity (see modules/ses-smtp-user)."
  type        = string
  default     = "factoriosurvivalworld.com"
}

variable "mail_sender" {
  description = "From address of verification and password-reset mail."
  type        = string
  default     = "accounts-noreply@factoriosurvivalworld.com"
}

variable "smtp_iam_user_name" {
  type    = string
  default = "mmo-orion-keycloak-smtp"
}

# --- Clients -----------------------------------------------------------------

variable "zulip_host" {
  description = "Zulip's hostname: the only place a `zulip` login may redirect back to."
  type        = string
  default     = "chat.errantry.renownandruin.com"
}

variable "grafana_url" {
  description = "Grafana's root URL (the monitoring root's grafana_url): the only place a `grafana` login (staff realm) may redirect back to. Default: the VPN-only name, plain HTTP inside the operator VPN."
  type        = string
  default     = "http://grafana.vpn.errantry.renownandruin.com"
}

variable "game_web_origins" {
  description = "Origins the browser build of the game is served from (CORS for the `game` client's token calls)."
  type        = list(string)
  default     = ["https://game.errantry.renownandruin.com"]
}

variable "login_audience" {
  description = "Audience put into `game` access tokens; the game's login service accepts only tokens carrying it."
  type        = string
  default     = "mmo-login"
}

variable "site_url" {
  description = "Origin the static website is served from (no trailing slash): the `site` client's root URL, and the only place its login may redirect back to."
  type        = string
  default     = "https://www.errantry.renownandruin.com"
}
