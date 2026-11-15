# --- Placement ---------------------------------------------------------------

variable "namespace" {
  type    = string
  default = "monitoring"
}

variable "cluster_name" {
  description = "Prometheus external label `cluster` (Thanos dedupes / filters by it)."
  type        = string
  default     = "orion"
}

variable "storage_class" {
  description = "StorageClass of every PV (k3s local-path: no volume expansion — sizes below are final)."
  type        = string
  default     = "local-path"
}

# --- Object storage (Garage, modules/garage) ---------------------------------

variable "object_store" {
  description = "Where the telemetry lives: Garage's S3 API as the pods reach it (`endpoint` is host:port; `insecure` = plain HTTP, fine inside the cluster), its region name, and the bucket of each component. From the garage-buckets root."
  type = object({
    endpoint = string
    region   = string
    insecure = optional(bool, true)
    buckets = object({
      thanos    = string
      loki      = string
      tempo     = string
      pyroscope = string
    })
  })
}

variable "object_store_key" {
  description = "The access key that may read and write those buckets (garage-buckets root, tenant `telemetry`)."
  type = object({
    access_key = string
    secret_key = string
  })
  sensitive = true
}

# --- Grafana -----------------------------------------------------------------

variable "grafana_host" {
  type    = string
  default = "grafana.errantry.renownandruin.com"
}

variable "grafana_oidc" {
  description = "Log in to Grafana through an OpenID Connect identity provider (on orion: Keycloak's `staff` realm, client `grafana`): the realm's issuer URL, the client id, and the login button's label. Only people holding a grafana client role there get in. Null: the local `admin` login only."
  type = object({
    issuer       = string
    client_id    = string
    display_name = optional(string, "Staff account")
  })
  default = null
}

variable "grafana_password_login" {
  description = "Show Grafana's username / password form next to `grafana_oidc`. Off: the identity provider is the only way in through the browser, so when it is down nobody can log in until this is turned back on (the `admin` password still works on the API)."
  type        = bool
  default     = true
  validation {
    condition     = var.grafana_password_login || var.grafana_oidc != null
    error_message = "grafana_password_login = false without grafana_oidc leaves no way to log in."
  }
}

variable "grafana_oidc_client_secret" {
  description = "Client secret of grafana_oidc.client_id."
  type        = string
  default     = null
  sensitive   = true
}

variable "cluster_issuer" {
  description = "cert-manager ClusterIssuer (envs/orion/platform) for Grafana's certificate."
  type        = string
  default     = "letsencrypt"
}

variable "ingress_class" {
  type    = string
  default = "traefik"
}

variable "manage_dns_record" {
  description = "Create the Route53 A record grafana_host -> node_ip here (the zone is dns_zone)."
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

# --- Alerts ------------------------------------------------------------------

variable "cpu_alert_threshold_percent" {
  description = "NodeCpuHigh fires when a node's 5-minute average CPU usage stays above this (percent of all cores)."
  type        = number
  default     = 60
}

variable "cpu_alert_for" {
  description = "How long the CPU usage must stay above the threshold before NodeCpuHigh fires (Prometheus duration)."
  type        = string
  default     = "10m"
}

variable "zulip_alert_webhook_url" {
  description = "Send every alert to Zulip: the URL of an `Incoming webhook` bot for the `Prometheus Alertmanager` integration (Zulip: Settings -> Bots -> add, then generate the integration URL for a channel; add `&desc=summary` to show each alert's summary). Holds the bot's API key. Null: alerts are only visible in Grafana / Alertmanager."
  type        = string
  default     = null
  sensitive   = true
}

# --- Retention ---------------------------------------------------------------

variable "prometheus_retention" {
  description = "Local Prometheus retention; history lives in Thanos."
  type        = string
  default     = "2d"
}

variable "prometheus_retention_size" {
  type    = string
  default = "20GB"
}

variable "thanos_retention_raw" {
  type    = string
  default = "30d"
}

variable "thanos_retention_5m" {
  type    = string
  default = "90d"
}

variable "thanos_retention_1h" {
  type    = string
  default = "365d"
}

variable "loki_retention" {
  type    = string
  default = "30d"
}

variable "tempo_retention" {
  description = "Tempo block retention (Go duration)."
  type        = string
  default     = "336h" # 14d
}

variable "pyroscope_retention" {
  type    = string
  default = "30d"
}

# --- Volumes (local-path cannot expand: pick the size up front) --------------

variable "volume_sizes" {
  type = map(string)
  default = {
    prometheus       = "30Gi"
    alertmanager     = "1Gi"
    grafana          = "2Gi"
    thanos_store     = "10Gi"
    thanos_compactor = "80Gi"
    loki             = "10Gi"
    tempo            = "10Gi"
    pyroscope        = "10Gi"
  }
}

# --- Chart / image versions --------------------------------------------------

variable "chart_versions" {
  type = map(string)
  default = {
    kube_prometheus_stack = "92.21.2" # prometheus-community
    loki                  = "21.0.0"  # grafana-community (OSS Loki chart moved there 2026-03)
    tempo                 = "3.2.1"   # grafana-community (grafana/tempo is deprecated)
    pyroscope             = "2.4.3"   # grafana
    alloy                 = "1.15.0"  # grafana
  }
}

variable "thanos_image" {
  type    = string
  default = "quay.io/thanos/thanos:v0.42.5"
}
