terraform {
  required_version = ">= 1.10" # S3 backend `use_lockfile`
  required_providers {
    aws = { source = "hashicorp/aws", version = "~> 6.0" }
  }
}
