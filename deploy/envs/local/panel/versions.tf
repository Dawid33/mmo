terraform {
  required_version = ">= 1.10"
  # The twin's state is as throwaway as its cluster: under target/twin/, removed by `just twin-down`.
  backend "local" {
    path = "../../../../target/twin/panel.tfstate"
  }
  required_providers {
    kubernetes = { source = "hashicorp/kubernetes", version = "~> 3.2" }
  }
}
