variable "kubeconfig" {
  description = "Kubeconfig of the local k0s-in-docker cluster (`just dev-cluster-up` writes it; recipes pass it as TF_VAR_kubeconfig)."
  type        = string
}

provider "kubernetes" {
  config_path = var.kubeconfig
}
