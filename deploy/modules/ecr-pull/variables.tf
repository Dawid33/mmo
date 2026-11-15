variable "namespace" {
  description = "Namespace the pull secret and its refresher live in (the module does not create it)."
  type        = string
}

variable "secret_name" {
  description = "Name of the pull secret (kubernetes.io/dockerconfigjson) the namespace's pods list in imagePullSecrets."
  type        = string
  default     = "ecr-pull"
}

variable "registry_host" {
  description = "The ECR registry host (<account>.dkr.ecr.<region>.amazonaws.com): the part of a repository URL before the first `/`."
  type        = string
}

variable "ecr_region" {
  description = "Region of the ECR registry (the refresher asks it for pull tokens)."
  type        = string
}

variable "ecr_pull_access_key_id" {
  description = "Access key id of the pull-only IAM user (the registry root's output); the refresher's credentials."
  type        = string
}

variable "ecr_pull_secret_access_key" {
  description = "Its secret."
  type        = string
  sensitive   = true
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
