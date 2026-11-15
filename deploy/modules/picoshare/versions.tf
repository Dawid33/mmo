terraform {
  required_version = ">= 1.10"
  required_providers {
    kubernetes = { source = "hashicorp/kubernetes", version = "~> 3.2" }
    random     = { source = "hashicorp/random", version = "~> 3.7" }
  }
}
