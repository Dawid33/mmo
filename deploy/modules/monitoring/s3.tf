# Telemetry object storage: one bucket per component in Garage, the object store
# on this same server (modules/garage). The buckets, their expiry backstop and
# the access key are made by the garage-buckets root (tenant `telemetry`) and
# come in as var.object_store / var.object_store_key; here they only become two
# Kubernetes Secrets. The components talk to Garage inside the cluster, in plain
# HTTP and path style (http://<endpoint>/<bucket>/<key>).

locals {
  components = ["thanos", "loki", "tempo", "pyroscope"]
  buckets    = { for c in local.components : c => var.object_store.buckets[c] }
}

# The stack kept its data in AWS S3 until 2026-11. Those buckets are left as
# they are, with their data and their expiry rules: tofu only forgets them.
# The IAM user that wrote to them is destroyed with this change. Once nothing
# is wanted from them: aws s3 rb --force s3://mmo-orion-<component>-<account id>.
removed {
  from = aws_s3_bucket.telemetry
  lifecycle {
    destroy = false
  }
}

removed {
  from = aws_s3_bucket_public_access_block.telemetry
  lifecycle {
    destroy = false
  }
}

removed {
  from = aws_s3_bucket_ownership_controls.telemetry
  lifecycle {
    destroy = false
  }
}

removed {
  from = aws_s3_bucket_server_side_encryption_configuration.telemetry
  lifecycle {
    destroy = false
  }
}

removed {
  from = aws_s3_bucket_lifecycle_configuration.telemetry
  lifecycle {
    destroy = false
  }
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
    AWS_ACCESS_KEY_ID     = var.object_store_key.access_key
    AWS_SECRET_ACCESS_KEY = var.object_store_key.secret_key
    AWS_REGION            = var.object_store.region
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
        bucket             = local.buckets["thanos"]
        endpoint           = var.object_store.endpoint
        region             = var.object_store.region
        insecure           = var.object_store.insecure
        bucket_lookup_type = "path"
        # Garage on spinning disks can take minutes to finish a large multipart
        # upload (a block's 512 MB chunk file). Thanos's default wait is 2m; when
        # it runs out Thanos asks again and gets NoSuchUpload for a finished upload.
        http_config = { response_header_timeout = "15m" }
        access_key  = var.object_store_key.access_key
        secret_key  = var.object_store_key.secret_key
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
  count = var.manage_dns_record && var.grafana_public ? 1 : 0
  name  = var.dns_zone
}

resource "aws_route53_record" "grafana" {
  count   = var.manage_dns_record && var.grafana_public ? 1 : 0
  zone_id = data.aws_route53_zone.site[0].zone_id
  name    = var.grafana_host
  type    = "A"
  ttl     = 300
  records = [var.node_ip]
}
