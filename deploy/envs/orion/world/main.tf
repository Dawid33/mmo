# One game world on the orion k3s server: Errantry at game.errantry.renownandruin.com.
# Needs ../postgres (the database), ../login (the service token), ../registry
# (images and the pull user), ../platform (cert-manager) and ../monitoring (the
# ServiceMonitor kind), and the DNS record for game.errantry.renownandruin.com
# (the client certificate is issued by Let's Encrypt). Both server images must
# be in ECR (mmo/server, mmo/web, tag latest) before the apply. Order:
#   registry -> platform -> garage-buckets -> postgres -> dns -> world
#   just orion-init world && just orion-plan world && just orion-apply world
# The coordinator and the hosts stay unready until the world is forged from the
# toolbox (see modules/world/main.tf). Releases: scripts/world_update.sh.
# Hosts are not OpenTofu's: the coordinator keeps between fleet.min and fleet.max of them
# in the table desired_hosts and the reconciler (Deployment fleet) makes one Deployment
# host-<name> per row, on the coordinator's image of that moment, with the node ports of
# its slot (fleet.port_base + 2·slot, + 1; output client_ports is the range to open).
# An apply owns the coordinator's replica count (the module's coordinator_replicas, 2,
# with a PodDisruptionBudget): an `orion-apply world` while the world is stopped (`just
# world-stop`) or in the middle of an update STARTS the coordinator again on whatever
# image is set then (an interrupted update's half-set images included), not the hosts;
# images are never changed by an apply (ignore_changes). An apply that changes anything
# the coordinator AND the hosts share (database_url, service_token, the coordinator's
# certificate, save_secs, super_region_regions, ownership) restarts the coordinator and
# changes the host template: make it with the world STOPPED: `just world-stop`, apply,
# `just world-start`. A change of the host template alone (a host's resources) restarts
# no running host: each takes it at its next stop and start. Removing a host by hand:
# `just world-drain <name>`; the coordinator then removes its row and the reconciler its
# Deployment. Emptying a node: modules/world/hosts.tf.
variable "state_bucket" {
  description = "The orion state bucket (../backend.s3.tfbackend), to read other roots' outputs."
  type        = string
  default     = "mmo-orion-tfstate-029905505680"
}

variable "world_id" {
  type    = string
  default = "errantry"
}

variable "public_host" {
  type    = string
  default = "game.errantry.renownandruin.com"
}

variable "image_tag" {
  description = "Tag the workloads are created with (releases set digests with scripts/world_update.sh)."
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

data "terraform_remote_state" "postgres" {
  backend = "s3"
  config = {
    bucket = var.state_bucket
    region = var.aws_region
    key    = "orion/postgres.tfstate"
  }
}

data "terraform_remote_state" "login" {
  backend = "s3"
  config = {
    bucket = var.state_bucket
    region = var.aws_region
    key    = "orion/login.tfstate"
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
  repos = data.terraform_remote_state.registry.outputs.repository_urls
  # ecr-pull's default secret_name. module.world creates the namespace and
  # ecr_pull needs it, so the pods cannot take the name from ecr_pull's output
  # (a cycle): they retry their pulls until the secret exists, seconds later.
  pull_secret = "ecr-pull"
}

module "world" {
  source = "../../../modules/world"

  world_id = var.world_id
  images = {
    server = "${local.repos["mmo/server"]}:${var.image_tag}"
    web    = "${local.repos["mmo/web"]}:${var.image_tag}"
  }
  image_pull_secret = local.pull_secret
  database_url      = data.terraform_remote_state.postgres.outputs.world_urls[var.world_id]
  service_token     = data.terraform_remote_state.login.outputs.service_token

  # One host at least, two at most: node ports 30466-30469.
  fleet              = { min = 1, max = 2, port_base = 30466 }
  public_host        = var.public_host
  client_certificate = { issuer = "letsencrypt" }
  web = {
    ingress_host   = var.public_host
    login_upstream = "http://login.login.svc.cluster.local"
  }
  auth = { login_url = "http://login.login.svc.cluster.local" }

  resources = {
    coordinator = { cpu = "100m", memory = "512Mi" }
    host        = { cpu = "1", memory = "2Gi" }
    toolbox     = { memory = "1Gi" }
  }
}

module "ecr_pull" {
  source = "../../../modules/ecr-pull"
  providers = {
    aws.ecr = aws.ecr_pull
  }
  namespace                  = module.world.namespace
  secret_name                = local.pull_secret
  registry_host              = data.terraform_remote_state.registry.outputs.registry_host
  ecr_region                 = data.terraform_remote_state.registry.outputs.region
  ecr_pull_access_key_id     = data.terraform_remote_state.registry.outputs.pull_access_key_id
  ecr_pull_secret_access_key = data.terraform_remote_state.registry.outputs.pull_secret_access_key
}

output "namespace" {
  value = module.world.namespace
}

output "login_link" {
  description = "For `login world add … --link` (the coordinator's HTTPS API URL)."
  value       = module.world.login_link
}

output "coordinator_cert_pem" {
  value = module.world.coordinator_cert_pem
}

output "client_ports" {
  value = module.world.client_ports
}
