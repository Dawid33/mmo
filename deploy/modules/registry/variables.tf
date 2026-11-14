variable "repositories" {
  description = "ECR repository names. A new image (the game server, later) is a new entry; the pull user may pull from every one of them."
  type        = list(string)
  default     = ["mmo/login"]
}

variable "keep_images" {
  description = "Images kept per repository; older ones expire (never the image tagged `bootstrap`)."
  type        = number
  default     = 20
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
