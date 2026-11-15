variable "namespace" {
  type    = string
  default = "picoshare"
}

variable "host" {
  description = "Public hostname. Its DNS record (-> the node) is NOT made here: share.errantry.renownandruin.com is made by the `dns` root."
  type        = string
  default     = "share.errantry.renownandruin.com"
}

variable "image" {
  description = "PicoShare image (github.com/mtlynch/picoshare). Pinned; bump deliberately — the SQLite store migrates forward only."
  type        = string
  default     = "mtlynch/picoshare:v1.5.4"
}

variable "shared_secret" {
  description = "The upload passphrase (PS_SHARED_SECRET). Null: one is generated (`just orion-share-password`)."
  type        = string
  default     = null
  sensitive   = true
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
  description = "StorageClass of the data volume (k3s local-path: no expansion, and it does not enforce the size)."
  type        = string
  default     = "local-path"
}

variable "volume_size" {
  description = "Data volume: one SQLite file holding every upload."
  type        = string
  default     = "50Gi"
}

variable "memory_limit" {
  type    = string
  default = "1Gi"
}
