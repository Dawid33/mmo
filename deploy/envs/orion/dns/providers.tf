variable "aws_region" {
  description = "Region of the AWS provider (Route 53 is global; the region only picks the API endpoint)."
  type        = string
  default     = "eu-central-1"
}

# Unused: this root has no kubernetes provider, but `just orion-plan` /
# `orion-apply` pass `-var kube_context=…` to every root, and an undeclared
# variable would fail them.
variable "kube_context" {
  description = "Unused here (no kubernetes provider); declared so the generic orion recipes work."
  type        = string
  default     = "orion"
}

# Credentials: the AWS default chain (AWS_PROFILE / ~/.aws), like the backend.
provider "aws" {
  region = var.aws_region
  default_tags {
    tags = { project = "mmo", stack = "orion-dns", managed-by = "opentofu" }
  }
}
