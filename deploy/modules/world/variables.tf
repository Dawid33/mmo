variable "namespace" {
  description = "Game namespace created by the platform module."
  type        = string
  default     = "mmo"
}

variable "db_instances" {
  description = "Postgres instances (1 = no replica; the single-node target has nowhere to put a second)."
  type        = number
  default     = 1
}

variable "db_storage_size" {
  type    = string
  default = "5Gi"
}

variable "game_image" {
  description = "Game server image. null = no game servers yet (the server still binds 127.0.0.1 and has no container image — multi-instance stage 3)."
  type        = string
  default     = null
}

variable "game_replicas" {
  description = "Game server pods. On one node more pods add no capacity (the server already uses every core); 2 only to exercise the cross-pod path."
  type        = number
  default     = 1
}

variable "game_env" {
  description = "Extra environment for the game server container (world file, thread counts, log level)."
  type        = map(string)
  default     = {}
}

variable "game_resources" {
  type    = object({ cpu = string, memory = string })
  default = { cpu = "2", memory = "4Gi" }
}
