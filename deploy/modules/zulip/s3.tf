# Zulip's S3 upload backend (zulip.readthedocs.io/en/stable/production/upload-backends.html)
# on Garage, the object store on this same server (modules/garage). The three
# buckets and the access key are made by the garage-buckets root (tenant
# `zulip`) and come in as var.object_store / var.object_store_key.
#   uploads  files attached to messages. Private: Zulip checks the reader and
#            serves the file itself (its nginx proxies and caches S3). That
#            nginx fetches `https://<S3 host>/…` and nothing else, which is why
#            Zulip uses Garage's PUBLIC address and not the in-cluster one.
#   avatars  avatars, organisation icons, custom emoji. World-readable BY
#            DESIGN: browsers load them straight from the bucket's website
#            address (var.object_store.avatars_url).
#   exports  organisation data exports. Private; without it Zulip would put
#            them in the world-readable avatars bucket.
#
# NOTHING here is backed up: Garage keeps one copy, on this server's disk.

locals {
  buckets = var.object_store.buckets
}

# Zulip kept its files in AWS S3 until 2026-11. Those buckets are left as they
# are (the avatars one still world-readable, so addresses cached by clients keep
# working): tofu only forgets them. The IAM user that wrote to them is destroyed
# with this change. Copy their objects across BEFORE applying it
# (`just orion-garage-import zulip`); once nothing is wanted from them:
# aws s3 rb --force s3://mmo-orion-zulip-<uploads|avatars|exports>-<account id>.
removed {
  from = aws_s3_bucket.zulip
  lifecycle {
    destroy = false
  }
}

removed {
  from = aws_s3_bucket_public_access_block.zulip
  lifecycle {
    destroy = false
  }
}

removed {
  from = aws_s3_bucket_ownership_controls.zulip
  lifecycle {
    destroy = false
  }
}

removed {
  from = aws_s3_bucket_server_side_encryption_configuration.zulip
  lifecycle {
    destroy = false
  }
}

removed {
  from = aws_s3_bucket_lifecycle_configuration.zulip
  lifecycle {
    destroy = false
  }
}

removed {
  from = aws_s3_bucket_policy.avatars_public_read
  lifecycle {
    destroy = false
  }
}

data "aws_route53_zone" "site" {
  count = var.manage_dns_record ? 1 : 0
  name  = var.dns_zone
}

resource "aws_route53_record" "zulip" {
  count   = var.manage_dns_record ? 1 : 0
  zone_id = data.aws_route53_zone.site[0].zone_id
  name    = var.host
  type    = "A"
  ttl     = 300
  records = [var.node_ip]
}
