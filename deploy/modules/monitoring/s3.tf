# Telemetry object storage: one AWS S3 bucket per component (own lifecycle
# rule each), an IAM user scoped to them, and its key as two Kubernetes Secrets.

data "aws_caller_identity" "current" {}

locals {
  components  = ["thanos", "loki", "tempo", "pyroscope"]
  buckets     = { for c in local.components : c => "${var.bucket_prefix}-${c}-${data.aws_caller_identity.current.account_id}" }
  s3_endpoint = "s3.${var.aws_region}.amazonaws.com"
}

resource "aws_s3_bucket" "telemetry" {
  for_each = local.buckets
  bucket   = each.value
  # Telemetry is disposable: `tofu destroy` may empty and delete the bucket.
  force_destroy = true
}

resource "aws_s3_bucket_public_access_block" "telemetry" {
  for_each                = local.buckets
  bucket                  = aws_s3_bucket.telemetry[each.key].id
  block_public_acls       = true
  block_public_policy     = true
  ignore_public_acls      = true
  restrict_public_buckets = true
}

resource "aws_s3_bucket_ownership_controls" "telemetry" {
  for_each = local.buckets
  bucket   = aws_s3_bucket.telemetry[each.key].id
  rule {
    object_ownership = "BucketOwnerEnforced"
  }
}

resource "aws_s3_bucket_server_side_encryption_configuration" "telemetry" {
  for_each = local.buckets
  bucket   = aws_s3_bucket.telemetry[each.key].id
  rule {
    apply_server_side_encryption_by_default {
      sse_algorithm = "AES256"
    }
  }
}

# No versioning (telemetry is disposable); expiry is a backstop past retention.
resource "aws_s3_bucket_lifecycle_configuration" "telemetry" {
  for_each = local.buckets
  bucket   = aws_s3_bucket.telemetry[each.key].id
  rule {
    id     = "expire-backstop"
    status = "Enabled"
    filter {}
    expiration {
      days = var.bucket_expiry_days[each.key]
    }
    abort_incomplete_multipart_upload {
      days_after_initiation = 2
    }
  }
}

resource "aws_iam_user" "telemetry" {
  name = var.iam_user_name
  tags = { purpose = "orion k3s telemetry - deploy/modules/monitoring" }
}

data "aws_iam_policy_document" "telemetry" {
  statement {
    sid       = "ListTelemetryBuckets"
    actions   = ["s3:ListBucket", "s3:GetBucketLocation", "s3:ListBucketMultipartUploads"]
    resources = [for k, _ in local.buckets : aws_s3_bucket.telemetry[k].arn]
  }
  statement {
    sid = "ReadWriteTelemetryObjects"
    actions = [
      "s3:GetObject",
      "s3:PutObject",
      "s3:DeleteObject",
      "s3:AbortMultipartUpload",
      "s3:ListMultipartUploadParts",
    ]
    resources = [for k, _ in local.buckets : "${aws_s3_bucket.telemetry[k].arn}/*"]
  }
}

resource "aws_iam_user_policy" "telemetry" {
  name   = "telemetry-buckets"
  user   = aws_iam_user.telemetry.name
  policy = data.aws_iam_policy_document.telemetry.json
}

# Rotate: tofu apply -replace=module.monitoring.aws_iam_access_key.telemetry
# (then restart the pods so they read the new Secret).
resource "aws_iam_access_key" "telemetry" {
  user = aws_iam_user.telemetry.name
}

resource "kubernetes_namespace_v1" "monitoring" {
  metadata {
    name = var.namespace
  }
}

# Env-var form, for Loki / Tempo / Pyroscope (their S3 clients fall back to the
# AWS env credential chain when the config carries no static key).
resource "kubernetes_secret_v1" "s3_env" {
  metadata {
    name      = "telemetry-s3"
    namespace = kubernetes_namespace_v1.monitoring.metadata[0].name
  }
  data = {
    AWS_ACCESS_KEY_ID     = aws_iam_access_key.telemetry.id
    AWS_SECRET_ACCESS_KEY = aws_iam_access_key.telemetry.secret
    AWS_REGION            = var.aws_region
  }
}

# Thanos objstore config (sidecar via the Prometheus CR, store gateway, compactor).
resource "kubernetes_secret_v1" "thanos_objstore" {
  metadata {
    name      = "thanos-objstore"
    namespace = kubernetes_namespace_v1.monitoring.metadata[0].name
  }
  data = {
    "objstore.yml" = yamlencode({
      type = "S3"
      config = {
        bucket     = local.buckets["thanos"]
        endpoint   = local.s3_endpoint
        region     = var.aws_region
        access_key = aws_iam_access_key.telemetry.id
        secret_key = aws_iam_access_key.telemetry.secret
      }
    })
  }
}

resource "random_password" "grafana_admin" {
  length  = 32
  special = false
}

resource "kubernetes_secret_v1" "grafana_admin" {
  metadata {
    name      = "grafana-admin"
    namespace = kubernetes_namespace_v1.monitoring.metadata[0].name
  }
  data = {
    admin-user     = "admin"
    admin-password = random_password.grafana_admin.result
  }
}

data "aws_route53_zone" "site" {
  count = var.manage_dns_record ? 1 : 0
  name  = var.dns_zone
}

resource "aws_route53_record" "grafana" {
  count   = var.manage_dns_record ? 1 : 0
  zone_id = data.aws_route53_zone.site[0].zone_id
  name    = var.grafana_host
  type    = "A"
  ttl     = 300
  records = [var.node_ip]
}
