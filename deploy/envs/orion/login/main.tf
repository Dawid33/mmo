# The game's login service on the orion k3s server: https://login.factoriosurvivalworld.com.
# Needs ../platform (cert-manager's ClusterIssuer), ../monitoring (the ServiceMonitor kind),
# ../keycloak and ../keycloak-realm applied first (the issuer comes from the realm root's state).
# The binary must be on the node BEFORE the first apply (`just orion-login-push`): the
# Deployment waits for its rollout, and a failed apply taints it.
#   just orion-login-push
#   just orion-init login && just orion-plan login && just orion-apply login
# Operator commands: `just orion-login …` (e.g. `just orion-login world list`).
variable "state_bucket" {
  description = "The orion state bucket (../backend.s3.tfbackend), to read the keycloak-realm root's outputs."
  type        = string
  default     = "mmo-orion-tfstate-029905505680"
}

data "terraform_remote_state" "keycloak_realm" {
  backend = "s3"
  config = {
    bucket = var.state_bucket
    region = var.aws_region
    key    = "orion/keycloak-realm.tfstate"
  }
}

module "login" {
  source = "../../../modules/login"
  issuer = data.terraform_remote_state.keycloak_realm.outputs.issuer
}

output "url" {
  value = module.login.url
}

output "service_token" {
  value     = module.login.service_token
  sensitive = true
}
