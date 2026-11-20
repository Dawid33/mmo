variable "namespace" {
  type    = string
  default = "pg-management"
}

variable "services" {
  description = "Services that receive one login role and one database each."
  type        = list(string)
  default     = ["login", "keycloak"]
  validation {
    condition     = alltrue([for service in var.services : can(regex("^[a-z][a-z0-9_]{0,30}$", service))])
    error_message = "A service name must be a lowercase letter followed by at most 30 lowercase letters, digits, or underscores."
  }
  validation {
    condition     = length(distinct(var.services)) == length(var.services)
    error_message = "Service names must be unique."
  }
}

variable "image" {
  description = "CloudNativePG operand image, also used for pg_dump."
  type        = string
  default     = "ghcr.io/cloudnative-pg/postgresql:17"
}

variable "storage_class" {
  type    = string
  default = "local-path"
}

variable "storage_size" {
  type    = string
  default = "20Gi"
}

variable "max_connections" {
  type    = number
  default = 100
}

variable "resources" {
  type = object({
    requests = optional(map(string), { cpu = "100m", memory = "256Mi" })
    limits   = optional(map(string), { memory = "1Gi" })
  })
  default = {}
}

variable "backup" {
  description = "Garage target of the nightly logical dumps; null disables backups."
  type = object({
    endpoint   = string
    bucket     = string
    access_key = string
    secret_key = string
  })
  default   = null
  nullable  = true
  sensitive = true
}

variable "backup_keep" {
  type    = number
  default = 7
  validation {
    condition     = var.backup_keep >= 1 && var.backup_keep == floor(var.backup_keep)
    error_message = "backup_keep must be a whole number >= 1."
  }
}

variable "backup_schedule" {
  type    = string
  default = "45 3 * * *"
}

variable "backup_image" {
  type    = string
  default = "alpine/k8s:1.37.1"
}

variable "backup_scratch_size" {
  type    = string
  default = "10Gi"
}
