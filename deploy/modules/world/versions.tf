terraform {
  required_version = ">= 1.10"
  required_providers {
    helm       = { source = "hashicorp/helm", version = "~> 3.3" }
    kubernetes = { source = "hashicorp/kubernetes", version = "~> 3.2" }
    # private_key_pem_pkcs8 (identities.tf) exists from 4.0.
    tls = { source = "hashicorp/tls", version = "~> 4.1" }
  }
}
