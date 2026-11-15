variable "kubeconfig" {
  description = "Kubeconfig holding the twin's context (`just twin-up` writes it into the default one)."
  type        = string
  default     = "~/.kube/config"
}

variable "kube_context" {
  description = "Always the k3d twin, never the current context."
  type        = string
  default     = "k3d-mmo"
}

provider "kubernetes" {
  config_path    = var.kubeconfig
  config_context = var.kube_context
}

provider "helm" {
  kubernetes = {
    config_path    = var.kubeconfig
    config_context = var.kube_context
  }
}
