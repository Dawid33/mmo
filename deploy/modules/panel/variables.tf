variable "namespace" {
  description = "Namespace the panel runs in; the module creates it."
  type        = string
  default     = "panel"
}

variable "image" {
  description = "The image the Deployment is CREATED with (mmo/panel). Releases set a digest with `kubectl set image` (scripts/orion_panel_push.sh) and the Deployment ignores later changes to its image."
  type        = string
}

variable "image_pull_secret" {
  description = "Name of a dockerconfigjson Secret in the namespace (the ecr-pull module's). null = none (image already on the node)."
  type        = string
  default     = null
}

variable "grafana_url" {
  description = "Grafana's base URL for the world pages' dashboard link (PANEL_GRAFANA_URL). null = no link."
  type        = string
  default     = null
}

variable "memory" {
  description = "The panel's memory request = limit."
  type        = string
  default     = "64Mi"
}
