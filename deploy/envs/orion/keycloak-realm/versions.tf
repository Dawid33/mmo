terraform {
  required_version = ">= 1.10"
  required_providers {
    aws      = { source = "hashicorp/aws", version = "~> 6.0" }
    keycloak = { source = "keycloak/keycloak", version = "~> 5.10" }
  }
}
