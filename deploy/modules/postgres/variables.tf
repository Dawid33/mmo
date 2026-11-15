variable "namespace" {
  type    = string
  default = "postgres"
}

variable "worlds" {
  description = "World ids: each gets a database and a login role `world_<id>`. Lowercase letters and digits only (the id is a role name, a database name and part of Kubernetes object names)."
  type        = list(string)
  validation {
    condition     = alltrue([for w in var.worlds : can(regex("^[a-z][a-z0-9]{0,30}$", w))])
    error_message = "A world id is a lowercase letter followed by up to 30 lowercase letters or digits."
  }
  validation {
    condition     = length(distinct(var.worlds)) == length(var.worlds)
    error_message = "World ids must be unique."
  }
}

variable "storage_class" {
  description = "StorageClass of the cluster's volume (k3s local-path: no expansion)."
  type        = string
}

variable "storage_size" {
  type    = string
  default = "20Gi"
}

variable "image" {
  description = "PostgreSQL image (CloudNativePG operand image). Also runs pg_dump in the backup. A major-version bump needs a dump and restore."
  type        = string
  default     = "ghcr.io/cloudnative-pg/postgresql:17"
}

variable "max_prepared_transactions" {
  description = "The server's two-phase world cuts need it > 0."
  type        = number
  default     = 64
}

variable "backup" {
  description = "S3 (Garage) target of the nightly pg_dump; null = no CronJob."
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
  description = "Dumps kept per world."
  type        = number
  default     = 7
}

variable "backup_image" {
  description = "Uploader image of the dump CronJob (AWS CLI + POSIX sh). Same pin as the login module's refresher."
  type        = string
  default     = "alpine/k8s:1.37.1"
}

variable "backup_schedule" {
  type    = string
  default = "15 3 * * *"
}

variable "resources" {
  description = "Resources of the PostgreSQL pod, in the Kubernetes resources shape."
  type = object({
    requests = optional(map(string), { cpu = "100m", memory = "256Mi" })
    limits   = optional(map(string), { memory = "1Gi" })
  })
  default = {}
}
