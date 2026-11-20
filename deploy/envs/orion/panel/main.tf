# The world panel on the orion k3s server (deploy/modules/panel): an operator page over
# every world namespace — state, pods, health, metrics, release history — that starts,
# stops and forges worlds. No Ingress: `just orion-panel` port-forwards it to
# http://127.0.0.1:8090/. Each world grants it access itself (the world module's
# `panel` variable, set in ../world). Needs ../registry applied with mmo/panel in its
# repositories and an image pushed (`just orion-panel-push`) before the first apply:
#   just orion-plan registry && just orion-apply registry
#   just orion-panel-push
#   just orion-init panel && just orion-plan panel && just orion-apply panel
# Releases: `just orion-panel-push` (set by digest, never an apply).
variable "state_bucket" {
  description = "The orion state bucket (../backend.s3.tfbackend), to read other roots' outputs."
  type        = string
  default     = "mmo-orion-tfstate-029905505680"
}

variable "image_tag" {
  description = "Tag the Deployment is created with (releases set a digest with scripts/orion_panel_push.sh)."
  type        = string
  default     = "latest"
}

data "terraform_remote_state" "registry" {
  backend = "s3"
  config = {
    bucket = var.state_bucket
    region = var.aws_region
    key    = "orion/registry.tfstate"
  }
}

# The pull-only user, for the apply-time pull token only (as the login root).
provider "aws" {
  alias      = "ecr_pull"
  region     = data.terraform_remote_state.registry.outputs.region
  access_key = data.terraform_remote_state.registry.outputs.pull_access_key_id
  secret_key = data.terraform_remote_state.registry.outputs.pull_secret_access_key
}

locals {
  # ecr-pull's default secret_name; module.panel creates the namespace ecr_pull needs (as
  # the world root: the pod retries its pull until the secret exists).
  pull_secret = "ecr-pull"
}

module "panel" {
  source = "../../../modules/panel"

  image             = "${data.terraform_remote_state.registry.outputs.repository_urls["mmo/panel"]}:${var.image_tag}"
  image_pull_secret = local.pull_secret
  grafana_url       = "http://grafana.vpn.errantry.renownandruin.com" # VPN-only (vpn-names root)
}

module "ecr_pull" {
  source = "../../../modules/ecr-pull"
  providers = {
    aws.ecr = aws.ecr_pull
  }
  namespace                  = module.panel.namespace
  secret_name                = local.pull_secret
  registry_host              = data.terraform_remote_state.registry.outputs.registry_host
  ecr_region                 = data.terraform_remote_state.registry.outputs.region
  ecr_pull_access_key_id     = data.terraform_remote_state.registry.outputs.pull_access_key_id
  ecr_pull_secret_access_key = data.terraform_remote_state.registry.outputs.pull_secret_access_key
}

output "namespace" {
  value = module.panel.namespace
}
