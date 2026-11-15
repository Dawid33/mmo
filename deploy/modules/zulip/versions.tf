terraform {
  required_version = ">= 1.10"
  required_providers {
    # aws.ses: the region of the SES identity (an SMTP password is tied to a region).
    aws        = { source = "hashicorp/aws", version = "~> 6.0", configuration_aliases = [aws.ses] }
    helm       = { source = "hashicorp/helm", version = "~> 3.3" }
    kubernetes = { source = "hashicorp/kubernetes", version = "~> 3.2" }
    random     = { source = "hashicorp/random", version = "~> 3.7" }
  }
}
