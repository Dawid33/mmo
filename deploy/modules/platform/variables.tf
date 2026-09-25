variable "game_namespace" {
  description = "Namespace the game servers (Agones GameServers) and the world database live in."
  type        = string
  default     = "mmo"
}

variable "single_node" {
  description = "One replica per operator component: a single-node cluster (local dev, the planned k0s box) has nowhere to spread them."
  type        = bool
  default     = true
}

variable "agones_version" {
  type    = string
  default = "1.60.0"
}

variable "agones_port_range" {
  description = "Host port range Agones allocates game server ports from. Must be open (UDP) on the node's firewall."
  type        = object({ min = number, max = number })
  default     = { min = 7000, max = 8000 }
}

variable "cnpg_chart_version" {
  description = "cloudnative-pg chart (0.29.0 = operator 1.30.0)."
  type        = string
  default     = "0.29.0"
}

variable "install_local_storage" {
  description = "Install the OpenEBS LocalPV hostpath provisioner as the default StorageClass. k0s ships none; a single node needs node-local volumes."
  type        = bool
  default     = true
}

variable "localpv_chart_version" {
  type    = string
  default = "4.6.0"
}
