variable "namespace" {
  type    = string
  default = "site"
}

variable "host" {
  description = "Public hostname. Its DNS record (-> the node) is NOT made here: it already exists in the survival-world infra (Route53)."
  type        = string
  default     = "factoriosurvivalworld.com"
}

variable "redirect_hosts" {
  description = "Other names that answer with a 301 to https://<host> (and are on the certificate)."
  type        = list(string)
  default     = ["www.factoriosurvivalworld.com"]
}

variable "site_dir" {
  description = "The static site (website/site): every file under it is served."
  type        = string
}

variable "issuer" {
  description = "The login realm's issuer URL (keycloak-realm root). Its origin is the page's only connect-src; js/config.js must name it."
  type        = string
}

variable "image" {
  description = "Stock nginx that runs as a non-root user and listens on 8080."
  type        = string
  default     = "nginxinc/nginx-unprivileged:1.28-alpine"
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
