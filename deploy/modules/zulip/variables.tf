# --- Placement ---------------------------------------------------------------

variable "namespace" {
  type    = string
  default = "zulip"
}

variable "chart_version" {
  description = "Zulip's own chart (oci://ghcr.io/zulip/helm-charts/zulip; it pins the server image). Bump deliberately and read docs/how-to/helm-upgrading.md in zulip/docker-zulip first: database migrations only go forward."
  type        = string
  default     = "2.4.0"
}

variable "storage_class" {
  description = "StorageClass of both PVs (k3s local-path: no volume expansion, and it does not enforce the size)."
  type        = string
  default     = "local-path"
}

variable "volume_sizes" {
  description = "PV sizes. `data` holds settings and, with auto_backup, the database dumps (uploads are in Garage); `postgres` is the chart's own PostgreSQL."
  type        = map(string)
  default = {
    data     = "20Gi"
    postgres = "20Gi"
  }
}

# --- Zulip -------------------------------------------------------------------

variable "host" {
  description = "Public hostname (EXTERNAL_HOST). Fixed once an organisation exists: every link and login is built from it."
  type        = string
  default     = "chat.errantry.renownandruin.com"
}

variable "admin_email" {
  description = "ZULIP_ADMINISTRATOR: where the server sends its error and support mail."
  type        = string
}

variable "proxy_cidrs" {
  description = "LOADBALANCER_IPS: the addresses Zulip trusts X-Forwarded-* from. Traefik reaches the pod from a pod IP, so this is the k3s pod range."
  type        = list(string)
  default     = ["10.42.0.0/16"]
}

variable "queue_workers_multiprocess" {
  description = "One process per queue worker (faster, about 1.5 GB more memory) instead of one multi-threaded process. Zulip picks from the HOST's memory when left to itself, so it is set here."
  type        = bool
  default     = false
}

variable "auto_backup" {
  description = "The image's daily `pg_dump` into /data/backups (03:30 UTC). Off: no backups for now. It is never pruned and stays on the same disk as the database, so turning it on needs pruning and off-box shipping with it."
  type        = bool
  default     = false
}

variable "memory_limits" {
  description = "Memory limit of each pod. The Bitnami sub-charts default to presets far too small for this (PostgreSQL: 192Mi)."
  type        = map(string)
  default = {
    zulip     = "6Gi"
    postgres  = "2Gi"
    rabbitmq  = "1Gi"
    redis     = "512Mi"
    memcached = "512Mi"
  }
}

variable "startup_minutes" {
  description = "How long the first boot (and an upgrade's migrations) may take before the pod is restarted."
  type        = number
  default     = 15
}

# --- Metrics -----------------------------------------------------------------

variable "metrics" {
  description = "Prometheus exporters + ServiceMonitors for PostgreSQL, RabbitMQ and Redis (not memcached: its exporter cannot authenticate against SASL). Needs the monitoring root applied first (the ServiceMonitor CRD); turn off on a cluster without it."
  type        = bool
  default     = true
}

variable "exporter_images" {
  description = "Exporter sidecar images (repository:tag). The sub-charts default to bitnami/*:latest, which moves daily; these are the last fixed versions, frozen in bitnamilegacy like the services' own images."
  type        = map(string)
  default = {
    postgres = "bitnamilegacy/postgres-exporter:0.17.1-debian-12-r16"
    redis    = "bitnamilegacy/redis-exporter:1.76.0-debian-12-r0"
  }
}

# --- Login -------------------------------------------------------------------

variable "oidc" {
  description = "Log in through an OpenID Connect identity provider (oidc.tf): its issuer URL, this Zulip's client id there, and the login button's label. The provider must allow the redirect https://<host>/complete/oidc/ and must verify email addresses. Null: Zulip's own email + password only."
  type = object({
    issuer       = string
    client_id    = string
    display_name = optional(string, "Game account")
  })
  default = null
}

variable "oidc_client_secret" {
  description = "Client secret of oidc.client_id."
  type        = string
  default     = null
  sensitive   = true
}

variable "password_login" {
  description = "Keep Zulip's own email + password login next to `oidc`. Turn off only AFTER an organisation owner has logged in through the provider once (same email there as in Zulip), or nobody can administer the organisation."
  type        = bool
  default     = true
}

# --- Outgoing mail -----------------------------------------------------------

variable "ses" {
  description = "Send mail through AWS SES (ses.tf): `domain` is an SES domain identity that ALREADY exists and is verified in the region of the `aws.ses` provider (factoriosurvivalworld.com is set up, with DKIM / MAIL FROM / DMARC, by the survival-world infra in ~/Projects/mod); `sender` is the one From address Zulip uses, at that domain. Null: use `smtp`, or send nothing."
  type = object({
    domain = string
    sender = string
  })
  default = null
}

variable "smtp" {
  description = "Outgoing SMTP (STARTTLS) when not using `ses`. Null (and no `ses`): Zulip cannot send mail, so invitations, sign-up confirmations and password resets do not work; the first organisation can still be made with `just orion-zulip-realm-link`. `noreply`: a fixed From address, for providers that only accept verified senders (SES)."
  type = object({
    host    = string
    user    = string
    port    = optional(number, 587)
    noreply = optional(string)
  })
  default = null
}

variable "smtp_password" {
  description = "Password of smtp.user."
  type        = string
  default     = null
  sensitive   = true
}

# --- Object storage (Garage, modules/garage) ---------------------------------

variable "object_store" {
  description = "Where uploads live: Garage's PUBLIC S3 address (`url`, https — Zulip's nginx fetches uploads over HTTPS only), its region name, the three buckets, and the public website address of the avatars bucket. From the garage-buckets root."
  type = object({
    url    = string
    region = string
    buckets = object({
      uploads = string
      avatars = string
      exports = string
    })
    avatars_url = string
  })
  validation {
    condition     = startswith(var.object_store.url, "https://") && startswith(var.object_store.avatars_url, "https://")
    error_message = "object_store.url and object_store.avatars_url must both be https:// addresses."
  }
}

variable "object_store_key" {
  description = "The access key that may read and write those buckets (garage-buckets root, tenant `zulip`)."
  type = object({
    access_key = string
    secret_key = string
  })
  sensitive = true
}

variable "iam_user_name" {
  description = "Name stem of the IAM user made here: `<name>-smtp`, the send-only SES login (ses.tf)."
  type        = string
  default     = "mmo-orion-zulip"
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
  description = "Create the Route53 A record host -> node_ip here. Turn off if the record is made in the survival-world infra (like Grafana's and PicoShare's); it must exist before the first apply either way, or the certificate cannot be issued."
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
