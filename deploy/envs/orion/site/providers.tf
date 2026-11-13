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
  description = "Region of the state bucket (only used to read the keycloak-realm root's outputs)."
  type        = string
  default     = "eu-central-1"
}

variable "state_bucket" {
  description = "The orion state bucket (../backend.s3.tfbackend)."
  type        = string
  default     = "mmo-orion-tfstate-029905505680"
}

provider "kubernetes" {
  config_path    = var.kubeconfig
  config_context = var.kube_context
}

# The realm's issuer URL, from the keycloak-realm root.
data "terraform_remote_state" "realm" {
  backend = "s3"
  config = {
    bucket = var.state_bucket
    region = var.aws_region
    key    = "orion/keycloak-realm.tfstate"
  }
}
