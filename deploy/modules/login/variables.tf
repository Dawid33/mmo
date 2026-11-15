variable "namespace" {
  type    = string
  default = "login"
}

variable "host" {
  description = "Public hostname of the login API (clients and the game's servers are pointed at it)."
  type        = string
  default     = "login.errantry.renownandruin.com"
}

variable "image_repository" {
  description = "The login image's ECR repository URL (<account>.dkr.ecr.<region>.amazonaws.com/mmo/login; the registry root's output)."
  type        = string
}

variable "image_tag" {
  description = "Tag the Deployment is CREATED with (`latest`, the repository's only tag). Releases set a digest with `kubectl set image` (`just orion-login-push`) and the Deployment ignores later changes to its image, so changing this after the first apply does nothing."
  type        = string
  default     = "latest"
}

variable "ecr" {
  description = "Pulling the image from the private ECR registry (ecr.tf): the registry's region and the pull-only IAM user's key (the registry root's outputs; the key is the refresher's credentials). null = no pull secret, no refresher and no imagePullSecrets: the image is already on the node (the k3d twin imports it)."
  type = object({
    region            = string
    access_key_id     = string
    secret_access_key = string
  })
  default = null
}

variable "refresh_image" {
  description = "Image of the pull-secret refresher (CronJob ecr-refresh): needs the AWS CLI, kubectl and a POSIX sh. Pinned; keep kubectl within one minor of the cluster."
  type        = string
  default     = "alpine/k8s:1.37.1"
}

variable "refresh_schedule" {
  description = "When the refresher rewrites the pull secret. An ECR token lives 12 hours."
  type        = string
  default     = "0 */6 * * *"
}

variable "issuer_host_alias_ip" {
  description = "An IP the issuer's host name is pinned to inside the pod. Set it to the Traefik Service's cluster IP only if the pod cannot reach the issuer's public address; that IP changes if Traefik is reinstalled."
  type        = string
  default     = null
}

variable "issuer" {
  description = "Keycloak realm issuer URL (VOXEL_OIDC_ISSUER), e.g. https://auth.errantry.renownandruin.com/realms/<realm>. The root passes it. null = no Keycloak login (VOXEL_OIDC_ISSUER unset: the k3d twin, which has no Keycloak)."
  type        = string
  default     = null
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
  default     = ["https://game.errantry.renownandruin.com"]
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
  default = "renownandruin.com"
}

variable "node_ip" {
  type    = string
  default = "5.9.7.138"
}
