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
# An apply owns the replica counts (coordinator 1, hosts = length(var.hosts)): an
# `orion-apply world` while the world is stopped (`just world-stop`) or in the middle
# of an update STARTS it again on whatever images are set then (an interrupted
# update's half-set images included). Apply only with the world running and no
# update under way; images are never changed by an apply (ignore_changes).
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

  hosts = [
    { quic_node_port = 30466, wt_node_port = 30467 },
    { quic_node_port = 30468, wt_node_port = 30469 },
  ]
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

output "host_link" {
  description = "For `login world add … --link` (the entry host's service link)."
  value       = module.world.host_link
}

output "host_link_cert_pem" {
  value = module.world.host_link_cert_pem
}

output "endpoints" {
  value = module.world.endpoints
}
