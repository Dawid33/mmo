# The game's login service on the orion k3s server: https://login.errantry.renownandruin.com.
# Needs ../platform (cert-manager's ClusterIssuer), ../monitoring (the ServiceMonitor kind),
# ../keycloak and ../keycloak-realm applied first (the issuer comes from the realm root's state),
# and ../registry (the ECR repository and the pull user; read from its state). The image
# must be in ECR BEFORE the first apply: the Deployment waits for its rollout, and a
# failed apply taints it. Order:
#   just orion-init registry && just orion-plan registry && just orion-apply registry
#   just orion-login-push     # first push: no Deployment yet, only pushes <repo>:latest
#   just orion-init login && just orion-plan login && just orion-apply login
# Releases afterwards: `just orion-login-push` (kubectl set image by digest, not an apply).
# Operator commands: `just orion-login …` (e.g. `just orion-login world list`).
variable "state_bucket" {
  description = "The orion state bucket (../backend.s3.tfbackend), to read the keycloak-realm root's outputs."
  type        = string
  default     = "mmo-orion-tfstate-029905505680"
}

variable "issuer_host_alias_ip" {
  description = "Pin the issuer's host name to this IP inside the pod (the Traefik Service's cluster IP) only if the pod cannot reach the issuer's public address. Default: no alias."
  type        = string
  default     = null
}

variable "image_tag" {
  description = "Tag the Deployment is created with (later releases: `just orion-login-push`)."
  type        = string
  default     = "latest"
}

variable "use_management_db" {
  description = "Login's data lives in pg-management. Keep true: switching it off would point a normal deploy at the retired StatefulSet."
  type        = bool
  default     = true
}

variable "own_postgres" {
  description = "The retired login PostgreSQL is absent after the completed move. stopped is a pre-cleanup rehearsal state; running is a rollback-only override."
  type        = string
  default     = "absent"
}

data "terraform_remote_state" "registry" {
  backend = "s3"
  config = {
    bucket = var.state_bucket
    region = var.aws_region
    key    = "orion/registry.tfstate"
  }
}

# The pull-only user, for the apply-time pull token only (Secret login-ecr). A
# token made with the default chain's (administrator) credentials could push.
provider "aws" {
  alias      = "ecr_pull"
  region     = data.terraform_remote_state.registry.outputs.region
  access_key = data.terraform_remote_state.registry.outputs.pull_access_key_id
  secret_key = data.terraform_remote_state.registry.outputs.pull_secret_access_key
}

data "terraform_remote_state" "keycloak_realm" {
  backend = "s3"
  config = {
    bucket = var.state_bucket
    region = var.aws_region
    key    = "orion/keycloak-realm.tfstate"
  }
}

data "terraform_remote_state" "pg_management" {
  backend = "s3"
  config = {
    bucket = var.state_bucket
    region = var.aws_region
    key    = "orion/pg-management.tfstate"
  }
}

module "login" {
  source = "../../../modules/login"
  providers = {
    aws     = aws
    aws.ecr = aws.ecr_pull
  }
  issuer = data.terraform_remote_state.keycloak_realm.outputs.issuer

  image_repository = data.terraform_remote_state.registry.outputs.repository_url
  image_tag        = var.image_tag
  ecr = {
    region            = data.terraform_remote_state.registry.outputs.region
    access_key_id     = data.terraform_remote_state.registry.outputs.pull_access_key_id
    secret_access_key = sensitive(data.terraform_remote_state.registry.outputs.pull_secret_access_key)
  }

  issuer_host_alias_ip = var.issuer_host_alias_ip
  database             = var.use_management_db ? data.terraform_remote_state.pg_management.outputs.databases["login"] : null
  own_postgres         = var.own_postgres
  telemetry = {
    environment = "production"
    cluster     = "orion"
  }
}

output "url" {
  value = module.login.url
}

output "service_token" {
  value     = module.login.service_token
  sensitive = true
}
