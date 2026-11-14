variable "namespace" {
  type    = string
  default = "login"
}

variable "host" {
  description = "Public hostname of the login API (clients and the game's servers are pointed at it)."
  type        = string
  default     = "login.factoriosurvivalworld.com"
}

variable "image" {
  description = "Stock base image the node's login binary runs in (glibc, no application code). Must be at least as new as the glibc the binary was built against."
  type        = string
  default     = "debian:trixie-slim"
}

variable "command" {
  description = "The container's command: the binary itself, never a shell (a missing binary is then a StartError naming the path). `serve` is appended."
  type        = list(string)
  default     = ["/srv/mmo/bin/login"]
}

variable "bin_host_path" {
  description = "Directory on the node holding the release `login` binary (`just orion-login-push` fills it). Mounted read-only at /srv/mmo/bin."
  type        = string
  default     = "/srv/mmo/bin"
}

variable "issuer" {
  description = "Keycloak realm issuer URL (VOXEL_OIDC_ISSUER), e.g. https://auth.factoriosurvivalworld.com/realms/<realm>. The root passes it."
  type        = string
}

variable "oidc_audience" {
  description = "Audience the Keycloak access tokens must carry (VOXEL_OIDC_AUDIENCE)."
  type        = string
  default     = "mmo-login"
}

variable "oidc_client_id" {
  description = "Keycloak client the game logs in with (VOXEL_OIDC_CLIENT_ID)."
  type        = string
  default     = "game"
}

variable "cors_origins" {
  description = "Browser origins allowed to call the API (VOXEL_LOGIN_CORS_ORIGINS)."
  type        = list(string)
  default     = ["https://orion.factoriosurvivalworld.com"]
}

variable "postgres_image" {
  description = "The login service's own PostgreSQL. A major-version bump needs a dump and restore, not just a new tag."
  type        = string
  default     = "postgres:17-alpine"
}

variable "storage_class" {
  description = "StorageClass of the database volume (k3s local-path: no expansion, and it does not enforce the size)."
  type        = string
  default     = "local-path"
}

variable "postgres_volume_size" {
  description = "Final: local-path volumes cannot grow."
  type        = string
  default     = "2Gi"
}

variable "replicas" {
  description = "Login pods. Two keep logins up through a deploy or a crashed pod; the node and the single PostgreSQL stay single points. A PodDisruptionBudget exists only above 1."
  type        = number
  default     = 2
}

variable "metrics" {
  description = "A ServiceMonitor for /metrics. Needs the monitoring root applied first (the ServiceMonitor CRD)."
  type        = bool
  default     = true
}

variable "memory_limits" {
  description = "Memory limit of each pod."
  type        = map(string)
  default = {
    login    = "128Mi"
    postgres = "256Mi"
  }
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
  default = "factoriosurvivalworld.com"
}

variable "node_ip" {
  type    = string
  default = "5.9.7.138"
}
