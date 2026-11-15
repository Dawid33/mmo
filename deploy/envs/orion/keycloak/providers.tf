variable "kubeconfig" {
  description = "Kubeconfig holding the orion context."
  type        = string
  default     = "~/.kube/config"
}

variable "kube_context" {
  description = "Always the orion server, never the current context."
  type        = string
  default     = "orion"
}

variable "aws_region" {
  description = "Only Route53 (a global service) is used here."
  type        = string
  default     = "eu-central-1"
}

# Credentials: the AWS default chain (AWS_PROFILE / ~/.aws), like the backend.
provider "aws" {
  region = var.aws_region
  default_tags {
    tags = { project = "mmo", stack = "orion-keycloak", managed-by = "opentofu" }
  }
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
