terraform {
  required_version = ">= 1.10"
  required_providers {
    # aws.ecr: credentials of the pull-only user, for the apply-time pull token.
    aws        = { source = "hashicorp/aws", version = "~> 6.0", configuration_aliases = [aws.ecr] }
    helm       = { source = "hashicorp/helm", version = "~> 3.3" }
    kubernetes = { source = "hashicorp/kubernetes", version = "~> 3.2" }
    random     = { source = "hashicorp/random", version = "~> 3.7" }
    tls        = { source = "hashicorp/tls", version = "~> 4.1" }
  }
}
