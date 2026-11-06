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

# --- Object storage (AWS S3) -------------------------------------------------

variable "aws_region" {
  description = "Region of the telemetry buckets (Frankfurt: nearest to the Hetzner box)."
  type        = string
  default     = "eu-central-1"
}

variable "bucket_prefix" {
  description = "Telemetry bucket prefix: <prefix>-<component>-<account id>."
  type        = string
  default     = "mmo-orion"
}

variable "iam_user_name" {
  description = "IAM user whose access key the components use (S3 on those buckets only)."
  type        = string
  default     = "mmo-orion-telemetry"
}

# --- Grafana -----------------------------------------------------------------

variable "grafana_host" {
  type    = string
  default = "grafana.factoriosurvivalworld.com"
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
  description = "Create the Route53 A record grafana_host -> node_ip here. Off: the record already exists in the survival-world infra (aws_route53_record.monitoring)."
  type        = bool
  default     = false
}

variable "dns_zone" {
  type    = string
  default = "factoriosurvivalworld.com"
}

variable "node_ip" {
  type    = string
  default = "5.9.7.138"
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

# S3 lifecycle expiry (days) per bucket: a backstop a little past each
# component's own retention — the components' compactors do the real deletion.
variable "bucket_expiry_days" {
  type = map(number)
  default = {
    thanos    = 400
    loki      = 35
    tempo     = 17
    pyroscope = 35
  }
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
