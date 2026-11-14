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
  description = "Route53 (global) and the state bucket. The ECR token comes from the registry root's region (provider aws.ecr_pull in main.tf)."
  type        = string
  default     = "eu-central-1"
}

# Credentials: the AWS default chain (AWS_PROFILE / ~/.aws), like the backend.
provider "aws" {
  region = var.aws_region
  default_tags {
    tags = { project = "mmo", stack = "orion-login", managed-by = "opentofu" }
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
