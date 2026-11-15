# Zulip (team chat) on the orion k3s server: https://chat.errantry.renownandruin.com.
# Uploads in Garage (../garage + ../garage-buckets applied first), database in
# the chart's own PostgreSQL, mail through SES,
# login through Keycloak (keycloak_login; ../keycloak-realm applied first). Needs ../platform
# applied first (cert-manager's ClusterIssuer) and `admin_email` in
# terraform.tfvars (see terraform.tfvars.example).
#   just orion-init zulip && just orion-plan zulip && just orion-apply zulip
#   just orion-zulip-realm-link   # one-time link that creates the first organisation
variable "admin_email" {
  description = "Where the Zulip server sends its error and support mail."
  type        = string
}

variable "host" {
  type    = string
  default = "chat.errantry.renownandruin.com"
}

variable "manage_dns_record" {
  description = "Create the Route53 A record for `host` here (false: it is made in the survival-world infra)."
  type        = bool
  default     = true
}

variable "keycloak_login" {
  description = "Log in through Keycloak's `players` realm: reads the issuer, client id and client secret from the ../keycloak-realm root's state, so that root must be applied first. False: Zulip's own passwords only."
  type        = bool
  default     = false
}

variable "password_login" {
  description = "Keep Zulip's own email + password login next to Keycloak. Set false only after an owner has logged in through Keycloak once (modules/zulip/variables.tf)."
  type        = bool
  default     = true
}

variable "state_bucket" {
  description = "The orion state bucket (../backend.s3.tfbackend), to read the keycloak-realm root's outputs."
  type        = string
  default     = "mmo-orion-tfstate-029905505680"
}

data "terraform_remote_state" "keycloak_realm" {
  count   = var.keycloak_login ? 1 : 0
  backend = "s3"
  config = {
    bucket = var.state_bucket
    region = var.aws_region
    key    = "orion/keycloak-realm.tfstate"
  }
}

variable "ses" {
  description = "Outgoing mail through the SES domain identity of the survival-world infra (~/Projects/mod), as this From address. Null: `smtp` below, or no mail."
  type = object({
    domain = string
    sender = string
  })
  default = {
    domain = "factoriosurvivalworld.com"
    sender = "chat-noreply@factoriosurvivalworld.com"
  }
}

variable "smtp" {
  description = "Another SMTP server instead of SES (set ses = null); see modules/zulip/variables.tf."
  type = object({
    host    = string
    user    = string
    port    = optional(number, 587)
    noreply = optional(string)
  })
  default = null
}

variable "smtp_password" {
  type      = string
  default   = null
  sensitive = true
}

# Garage's public address, Zulip's buckets and their access key.
data "terraform_remote_state" "garage_buckets" {
  backend = "s3"
  config = {
    bucket = var.state_bucket
    region = var.aws_region
    key    = "orion/garage-buckets.tfstate"
  }
}

locals {
  garage = data.terraform_remote_state.garage_buckets.outputs
}

module "zulip" {
  source = "../../../modules/zulip"
  object_store = {
    url    = local.garage.s3_url
    region = local.garage.s3_region
    # The `zulip` tenant's buckets are zulip-uploads, zulip-avatars, zulip-exports.
    buckets = {
      for name, tenant in local.garage.buckets : trimprefix(name, "zulip-") => name if tenant == "zulip"
    }
    avatars_url = local.garage.website_urls["zulip-avatars"]
  }
  object_store_key = {
    access_key = local.garage.tenants["zulip"].access_key
    secret_key = local.garage.tenants["zulip"].secret_key
  }
  providers = {
    aws     = aws
    aws.ses = aws.ses
  }
  ses            = var.ses
  password_login = var.password_login
  oidc = var.keycloak_login ? {
    issuer    = data.terraform_remote_state.keycloak_realm[0].outputs.issuer
    client_id = data.terraform_remote_state.keycloak_realm[0].outputs.zulip_client_id
  } : null
  oidc_client_secret = var.keycloak_login ? data.terraform_remote_state.keycloak_realm[0].outputs.zulip_client_secret : null
  host               = var.host
  admin_email        = var.admin_email
  manage_dns_record  = var.manage_dns_record
  smtp               = var.smtp
  smtp_password      = var.smtp_password
}

output "url" {
  value = module.zulip.url
}

output "buckets" {
  value = module.zulip.buckets
}
