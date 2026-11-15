variable "kube_context" {
  description = "Unused: this root has no Kubernetes provider. Declared because `just orion-plan|apply` pass it to every root."
  type        = string
  default     = "orion"
}

variable "aws_region" {
  description = "Region of the state bucket."
  type        = string
  default     = "eu-central-1"
}

variable "state_bucket" {
  description = "The orion state bucket (../backend.s3.tfbackend), to read the garage root's outputs."
  type        = string
  default     = "mmo-orion-tfstate-029905505680"
}

# Where Garage's admin API is, and its token: from the garage root.
data "terraform_remote_state" "garage" {
  backend = "s3"
  config = {
    bucket = var.state_bucket
    region = var.aws_region
    key    = "orion/garage.tfstate"
  }
}

variable "garage_admin_url" {
  description = "Override of the admin API's address. Unset = the garage root's admin_url, its in-cluster name, reached over the operator VPN. VPN down: run `just orion-garage-admin` and pass `-var garage_admin_url=http://127.0.0.1:3903`."
  type        = string
  default     = null
}

# The admin API is not public: it is reached over the operator VPN
# (docs/superpowers/plans/2026-11-15-orion-vpn.md), or a port-forward (above).
provider "garage" {
  endpoint = coalesce(var.garage_admin_url, data.terraform_remote_state.garage.outputs.admin_url)
  token    = data.terraform_remote_state.garage.outputs.admin_token
}
