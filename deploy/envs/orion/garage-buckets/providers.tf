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

# The admin API is not public: `just orion-garage-admin` must be running (it
# forwards http://127.0.0.1:3903) for every plan and apply of this root.
provider "garage" {
  endpoint = data.terraform_remote_state.garage.outputs.admin_url
  token    = data.terraform_remote_state.garage.outputs.admin_token
}
