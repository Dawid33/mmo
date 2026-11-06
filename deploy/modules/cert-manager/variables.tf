variable "cert_manager_version" {
  description = "jetstack/cert-manager chart version."
  type        = string
  default     = "v1.21.3"
}

variable "acme_email" {
  description = "Contact address for the Let's Encrypt account (optional; Let's Encrypt no longer sends expiry mail)."
  type        = string
  default     = ""
}

variable "acme_server" {
  description = "ACME directory. The staging directory (https://acme-staging-v02.api.letsencrypt.org/directory) for experiments: production rate-limits failed issuances."
  type        = string
  default     = "https://acme-v02.api.letsencrypt.org/directory"
}

variable "issuer_name" {
  description = "Name of the ClusterIssuer; Ingresses opt in with the annotation cert-manager.io/cluster-issuer: <name>."
  type        = string
  default     = "letsencrypt"
}

variable "ingress_class" {
  description = "IngressClass that serves the HTTP-01 challenge (k3s ships Traefik)."
  type        = string
  default     = "traefik"
}
