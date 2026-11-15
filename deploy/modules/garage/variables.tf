variable "namespace" {
  type    = string
  default = "garage"
}

variable "image" {
  description = "Garage image (garagehq.deuxfleurs.fr). Pinned; bump deliberately and read the release's migration notes — the metadata store migrates forward only."
  type        = string
  default     = "dxflrs/garage:v2.5.0"
}

variable "s3_region" {
  description = "Region name every S3 client must sign with (Garage's own, not an AWS region)."
  type        = string
  default     = "garage"
}

variable "s3_host" {
  description = "Public hostname of the S3 API. Public because Zulip's nginx fetches uploads from `https://<s3 host>/…` only; requests still need a key (buckets and keys: modules/garage-buckets)."
  type        = string
  default     = "s3.errantry.renownandruin.com"
}

variable "web_root_domain" {
  description = "A bucket with website access is served, read-only and without a key, at https://<bucket>.<web_root_domain>."
  type        = string
  default     = "s3web.errantry.renownandruin.com"
}

variable "website_buckets" {
  description = "Buckets that get a public website hostname (Ingress rule, certificate, DNS record). The bucket itself, and its website switch, are made in modules/garage-buckets."
  type        = list(string)
  default     = ["zulip-avatars"]
}

variable "cluster_issuer" {
  description = "cert-manager ClusterIssuer (envs/orion/platform) for the certificate."
  type        = string
  default     = "letsencrypt"
}

variable "ingress_class" {
  type    = string
  default = "traefik"
}

variable "storage_class" {
  description = "StorageClass of both volumes (k3s local-path: no expansion, it does not enforce the size, and deleting the claim deletes the data)."
  type        = string
  default     = "local-path"
}

variable "volume_sizes" {
  description = "`meta` is the LMDB metadata store (small, wants a fast disk); `data` holds every object."
  type        = map(string)
  default = {
    meta = "10Gi"
    data = "500Gi"
  }
}

variable "memory_limit" {
  type    = string
  default = "2Gi"
}

variable "bucket_exporter_image" {
  description = "A stock Python image for the per-bucket metrics exporter (exporter.tf; standard library only)."
  type        = string
  default     = "python:3.13-alpine"
}

variable "metrics" {
  description = "Metrics for Prometheus: a ServiceMonitor for Garage's own /metrics, and the per-bucket exporter with its ServiceMonitor (exporter.tf). Needs the Prometheus operator CRDs: the monitoring root."
  type        = bool
  default     = true
}

variable "manage_dns_records" {
  description = "Create the Route53 A records (s3_host and every website host -> node_ip) here. They must exist before the certificate can be issued."
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
