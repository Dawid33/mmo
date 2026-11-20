# The twin's login service: the live module with no ECR (the image mmo/login:twin is
# imported into the node by `just twin-images`), no Keycloak (no issuer), no
# ServiceMonitor and no DNS record. Its operator commands:
# `kubectl --context k3d-mmo -n login exec deploy/login -- login …`.

# The module declares aws and aws.ecr; with ecr = null and no DNS record nothing calls
# AWS. Placeholder credentials keep the provider from looking for real ones.
provider "aws" {
  region                      = "eu-central-1"
  access_key                  = "twin-unused"
  secret_key                  = "twin-unused"
  skip_credentials_validation = true
  skip_requesting_account_id  = true
  skip_metadata_api_check     = true
  skip_region_validation      = true
}

variable "use_management_db" {
  type    = bool
  default = false
}

variable "own_postgres" {
  type    = string
  default = "running"
}

data "terraform_remote_state" "pg_management" {
  backend = "local"
  config = {
    path = "../../../../target/twin/pg-management.tfstate"
  }
}

module "login" {
  source = "../../../modules/login"
  providers = {
    aws     = aws
    aws.ecr = aws
  }
  image_repository  = "mmo/login"
  image_tag         = "twin"
  ecr               = null
  issuer            = null
  host              = "login.localhost"
  cors_origins      = ["http://game.localhost:8480"]
  replicas          = 1
  metrics           = false
  manage_dns_record = false
  database          = var.use_management_db ? data.terraform_remote_state.pg_management.outputs.databases["login"] : null
  own_postgres      = var.own_postgres
  telemetry = {
    otlp_endpoint       = "http://host.k3d.internal:4318"
    trace_sampler_ratio = 1
    profile_enabled     = true
    profile_endpoint    = "http://host.k3d.internal:4040"
    environment         = "twin"
    cluster             = "k3d-mmo"
  }
}

output "service_token" {
  value     = module.login.service_token
  sensitive = true
}
