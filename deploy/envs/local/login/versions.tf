terraform {
  required_version = ">= 1.10"
  # The twin's state is as throwaway as its cluster: under target/twin/, removed by `just twin-down`.
  backend "local" {
    path = "../../../../target/twin/login.tfstate"
  }
  required_providers {
    # The login module declares aws (Route53) and aws.ecr (the pull token); the twin uses neither.
    aws        = { source = "hashicorp/aws", version = "~> 6.0" }
    helm       = { source = "hashicorp/helm", version = "~> 3.3" }
    kubernetes = { source = "hashicorp/kubernetes", version = "~> 3.2" }
    random     = { source = "hashicorp/random", version = "~> 3.7" }
    tls        = { source = "hashicorp/tls", version = "~> 4.1" }
  }
}
