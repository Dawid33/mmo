variable "namespace" {
  type    = string
  default = "keycloak"
}

variable "host" {
  description = "Public hostname: the issuer of every token is https://<host>/realms/<realm>, so it is fixed once anything trusts it."
  type        = string
  default     = "auth.errantry.renownandruin.com"
}

variable "image" {
  description = "Keycloak image. Pinned; bump deliberately (read the release's migration notes: the database migrates forward only). After a bump, re-diff the orion theme's template.ftl against the new `base` theme and run `just site-theme-check` with the new tag in deploy/dev/compose.yml."
  type        = string
  default     = "quay.io/keycloak/keycloak:26.8.2"
}

variable "themes" {
  description = "Folder themes: theme name -> { path relative to the theme -> file content }, mounted read-only at /opt/keycloak/themes/<name> from a ConfigMap (text files only, under 900000 bytes per theme). Keycloak caches themes, so any change restarts the pod (strategy Recreate: about a minute without logins)."
  type        = map(map(string))
  default     = {}
}

variable "site_url" {
  description = "The public site; set as ORION_SITE_URL for the theme's links back to it."
  type        = string
  default     = "https://www.errantry.renownandruin.com"
}

variable "features_disabled" {
  description = "Keycloak features to switch off (KC_FEATURES_DISABLED). The account console and its API are unused: profiles live on the site."
  type        = list(string)
  default     = ["account", "account-api"]
}

variable "postgres_image" {
  description = "Keycloak's own PostgreSQL. A major-version bump needs a dump and restore, not just a new tag."
  type        = string
  default     = "postgres:17-alpine"
}

variable "storage_class" {
  description = "StorageClass of the database volume (k3s local-path: no expansion, and it does not enforce the size)."
  type        = string
  default     = "local-path"
}

variable "postgres_volume_size" {
  type    = string
  default = "10Gi"
}

variable "memory_limits" {
  description = "Memory limit of each pod. Keycloak sizes its heap from this (70%); below 2Gi is not recommended for production."
  type        = map(string)
  default = {
    keycloak = "2Gi"
    postgres = "1Gi"
  }
}

variable "admin_port" {
  description = "With no admin_host: the local port the admin console and admin API are reached on (`just orion-keycloak-admin` forwards it). Keycloak is told this address (KC_HOSTNAME_ADMIN), so the console only works through that forward."
  type        = number
  default     = 8180
}

variable "admin_host" {
  description = "A VPN-only name for the admin console and admin API (the vpn-names root: it resolves to this Service only over the operator VPN), e.g. keycloak.vpn.errantry.renownandruin.com. Keycloak then serves TLS itself on 8443 (Service port 443) with a certificate from admin_issuer, and is told https://<admin_host> (KC_HOSTNAME_ADMIN). null = the admin_port forward. The browser console needs https or 127.0.0.1 (a secure context), hence the certificate."
  type        = string
  default     = null
}

variable "admin_issuer" {
  description = "ClusterIssuer of admin_host's certificate: the vpn-names root's DNS-01 issuer (HTTP-01 cannot prove a name nothing public answers for)."
  type        = string
  default     = "letsencrypt-dns"
}

variable "metrics" {
  description = "A ServiceMonitor for Keycloak's metrics (management port). Needs the monitoring root applied first (the ServiceMonitor CRD)."
  type        = bool
  default     = true
}

# --- Ingress / DNS -----------------------------------------------------------

variable "cluster_issuer" {
  description = "cert-manager ClusterIssuer (envs/orion/platform) for the certificate."
  type        = string
  default     = "letsencrypt"
}

variable "ingress_class" {
  type    = string
  default = "traefik"
}

variable "manage_dns_record" {
  description = "Create the Route53 A record host -> node_ip here. It must exist before the certificate can be issued."
  type        = bool
  default     = true
}

variable "dns_zone" {
  type    = string
  default = "renownandruin.com"
}

variable "node_ip" {
  type    = string
  default = "5.9.7.138"
}
