variable "chart_version" {
  description = "cloudnative-pg chart (0.29.0 = operator 1.30.0). Same pin as the platform module."
  type        = string
  default     = "0.29.0"
}

variable "replicas" {
  description = "Operator replicas: 1 on a single node, 2 elsewhere."
  type        = number
  default     = 1
}
