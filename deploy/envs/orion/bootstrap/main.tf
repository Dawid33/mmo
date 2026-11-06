# The OpenTofu state bucket of every `envs/orion/*` root (the k3s server
# 5.9.7.138, kube context `orion`).
#
# Chicken and egg: this root creates the bucket its own state lives in. It was
# applied once with local state (backend.tf moved aside), then
# `tofu init -migrate-state` moved that state to s3://<bucket>/orion/bootstrap.tfstate
# — no local state remains. Day to day: `just orion-init bootstrap`, `just orion-plan bootstrap`.
#
# Re-bootstrapping from nothing (bucket gone): move backend.tf aside, `tofu init`,
# `tofu apply`, put backend.tf back, `just orion-init bootstrap -migrate-state`.

provider "aws" {
  region = var.region
}

variable "region" {
  description = "AWS region of the state bucket (Frankfurt: nearest to the Hetzner box)."
  type        = string
  default     = "eu-central-1"
}

variable "bucket_prefix" {
  description = "State bucket name prefix; the account id is appended so the name is globally unique."
  type        = string
  default     = "mmo-orion-tfstate"
}

data "aws_caller_identity" "current" {}

resource "aws_s3_bucket" "state" {
  bucket = "${var.bucket_prefix}-${data.aws_caller_identity.current.account_id}"

  lifecycle {
    prevent_destroy = true
  }
}

resource "aws_s3_bucket_versioning" "state" {
  bucket = aws_s3_bucket.state.id
  versioning_configuration {
    status = "Enabled"
  }
}

resource "aws_s3_bucket_server_side_encryption_configuration" "state" {
  bucket = aws_s3_bucket.state.id
  rule {
    apply_server_side_encryption_by_default {
      sse_algorithm = "AES256"
    }
  }
}

resource "aws_s3_bucket_public_access_block" "state" {
  bucket                  = aws_s3_bucket.state.id
  block_public_acls       = true
  block_public_policy     = true
  ignore_public_acls      = true
  restrict_public_buckets = true
}

resource "aws_s3_bucket_ownership_controls" "state" {
  bucket = aws_s3_bucket.state.id
  rule {
    object_ownership = "BucketOwnerEnforced"
  }
}

# Old state versions pile up with versioning on; keep 90 days of history.
resource "aws_s3_bucket_lifecycle_configuration" "state" {
  bucket = aws_s3_bucket.state.id
  rule {
    id     = "expire-old-state-versions"
    status = "Enabled"
    filter {}
    noncurrent_version_expiration {
      noncurrent_days = 90
    }
    abort_incomplete_multipart_upload {
      days_after_initiation = 7
    }
  }
}

output "state_bucket" {
  value = aws_s3_bucket.state.bucket
}

output "region" {
  value = var.region
}
