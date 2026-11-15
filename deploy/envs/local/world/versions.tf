terraform {
  required_version = ">= 1.10"
  # The twin's state is as throwaway as its cluster: under target/twin/, removed by `just twin-down`.
  backend "local" {
    path = "../../../../target/twin/world.tfstate"
  }
  required_providers {
    helm       = { source = "hashicorp/helm", version = "~> 3.3" }
    kubernetes = { source = "hashicorp/kubernetes", version = "~> 3.2" }
    tls        = { source = "hashicorp/tls", version = "~> 4.1" }
  }
}
