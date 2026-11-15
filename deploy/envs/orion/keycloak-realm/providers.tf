variable "kube_context" {
  description = "Unused: this root has no Kubernetes provider. Declared because `just orion-plan|apply` pass it to every root."
  type        = string
  default     = "orion"
}

variable "aws_region" {
  description = "Region of the state bucket and of IAM calls."
  type        = string
  default     = "eu-central-1"
}

variable "ses_region" {
  description = "Region of the SES domain identity (the survival-world infra's `region`)."
  type        = string
  default     = "eu-west-1"
}

variable "state_bucket" {
  description = "The orion state bucket (../backend.s3.tfbackend), to read the keycloak root's outputs."
  type        = string
  default     = "mmo-orion-tfstate-029905505680"
}

# Credentials: the AWS default chain (AWS_PROFILE / ~/.aws), like the backend.
provider "aws" {
  region = var.aws_region
  default_tags {
    tags = { project = "mmo", stack = "orion-keycloak-realm", managed-by = "opentofu" }
  }
}

provider "aws" {
  alias  = "ses"
  region = var.ses_region
  default_tags {
    tags = { project = "mmo", stack = "orion-keycloak-realm", managed-by = "opentofu" }
  }
}

# Where Keycloak's admin API is (and its public URL): from the keycloak root.
data "terraform_remote_state" "keycloak" {
  backend = "s3"
  config = {
    bucket = var.state_bucket
    region = var.aws_region
    key    = "orion/keycloak.tfstate"
  }
}

variable "keycloak_client_id" {
  description = "Service-account client in Keycloak's `master` realm that this root acts as (see terraform.tfvars.example for how it is made)."
  type        = string
  default     = "opentofu"
}

variable "keycloak_client_secret" {
  description = "That client's secret (terraform.tfvars, git-ignored)."
  type        = string
  sensitive   = true
}

# The admin API is not public: `just orion-keycloak-admin` must be running
# (it forwards http://127.0.0.1:8180) for every plan and apply of this root.
# It logs in as a service account, not as the human `admin` user: that user has
# two-factor login, which a password grant cannot pass.
provider "keycloak" {
  url           = data.terraform_remote_state.keycloak.outputs.admin_url
  client_id     = var.keycloak_client_id
  client_secret = var.keycloak_client_secret
}
