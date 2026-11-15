variable "repositories" {
  description = "ECR repository names. A new image (the game server, later) is a new entry; the pull user may pull from every one of them."
  type        = list(string)
  default     = ["mmo/login", "mmo/server", "mmo/web"]
}

variable "untagged_expiry_days" {
  description = "Days after its push an untagged image (a release that `latest` has moved past) expires. The previous release stays pullable about that long: what `kubectl rollout undo` needs if the node's image cache does not hold it."
  type        = number
  default     = 1
}

variable "pull_user_name" {
  description = "IAM user the cluster pulls with (its key lives in a Secret of each consuming namespace)."
  type        = string
  default     = "orion-ecr-pull"
}

variable "purpose" {
  description = "Tag text of the pull user. IAM allows letters, digits, spaces and _ . : / = + - @ only (no parentheses)."
  type        = string
  default     = "orion k3s pulls images from ECR"
}
