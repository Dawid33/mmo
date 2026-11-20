variable "dns_zone" {
  description = "The Route 53 zone holding vpn_domain (looked up, not managed)."
  type        = string
  default     = "renownandruin.com"
}

variable "vpn_domain" {
  description = "Parent of every VPN-only name. VPN peers send lookups under it to CoreDNS (split DNS)."
  type        = string
  default     = "vpn.errantry.renownandruin.com"
}

variable "names" {
  description = "VPN-only name (the label under vpn_domain) -> the Service's in-cluster DNS name it resolves as."
  type        = map(string)
  default = {
    keycloak = "keycloak.keycloak.svc.cluster.local"
    grafana  = "kube-prometheus-stack-grafana.monitoring.svc.cluster.local"
  }
}

variable "issuer_name" {
  description = "Name of the DNS-01 ClusterIssuer; a Certificate for a VPN-only name uses it."
  type        = string
  default     = "letsencrypt-dns"
}

variable "acme_server" {
  description = "ACME directory (the staging one for experiments: production rate-limits failures)."
  type        = string
  default     = "https://acme-v02.api.letsencrypt.org/directory"
}

variable "aws_region" {
  description = "Region cert-manager's Route 53 client is configured with (Route 53 itself is global)."
  type        = string
  default     = "eu-central-1"
}

variable "iam_user_name" {
  description = "IAM user cert-manager acts as for the DNS-01 challenge."
  type        = string
  default     = "mmo-orion-cert-manager-dns01"
}

variable "cert_manager_namespace" {
  description = "Where cert-manager runs: a ClusterIssuer's secrets must live there."
  type        = string
  default     = "cert-manager"
}
