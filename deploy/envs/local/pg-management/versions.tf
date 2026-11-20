terraform {
  required_version = ">= 1.10"
  backend "local" {
    path = "../../../../target/twin/pg-management.tfstate"
  }
  required_providers {
    helm       = { source = "hashicorp/helm", version = "~> 3.3" }
    kubernetes = { source = "hashicorp/kubernetes", version = "~> 3.2" }
    random     = { source = "hashicorp/random", version = "~> 3.7" }
  }
}
