# Buckets and access keys in Garage (modules/garage), through its admin API.
#
# Garage has no bucket policies and no IAM. Access is a (key, bucket) pair with
# three switches: read, write, owner. So:
#   - a tenant's key gets read + write on that tenant's buckets;
#   - a bucket is public by having website access on (read-only, single objects);
#   - key `opentofu` owns every bucket and can do nothing else: `owner` is what
#     may set a bucket's lifecycle rules, and it reads and writes no objects.
#
# The provider has no lifecycle resource and the AWS provider cannot do it
# either (it waits for S3 to echo the rule back in AWS's form; Garage echoes its
# own), so the rules are put with the aws CLI. That step is one-way: tofu does
# not see a rule changed by hand, only a rule changed here.

locals {
  buckets = merge([for tenant, t in var.tenants : {
    for name, b in t.buckets : name => merge(b, { tenant = tenant })
  }]...)

  lifecycle = { for name, b in local.buckets : name => jsonencode({
    Rules = [merge(
      {
        ID                             = "backstop"
        Status                         = "Enabled"
        Filter                         = { Prefix = "" }
        AbortIncompleteMultipartUpload = { DaysAfterInitiation = var.abort_multipart_days }
      },
      b.expiry_days == null ? {} : { Expiration = { Days = b.expiry_days } },
    )]
  }) }
}

resource "garage_bucket" "this" {
  for_each     = local.buckets
  global_alias = each.key

  website_enabled = each.value.website
  # Both set whenever the website is on: the provider shows a change on every
  # plan otherwise. Neither file has to exist.
  website_index_document = each.value.website ? "index.html" : null
  website_error_document = each.value.website ? "error.html" : null
}

# Rotate: tofu apply -replace='module.buckets.garage_key.tenant["<tenant>"]'
# in this root, then apply the root that uses the key.
resource "garage_key" "tenant" {
  for_each = var.tenants
  name     = each.key
}

resource "garage_bucket_permission" "tenant" {
  for_each      = local.buckets
  bucket_id     = garage_bucket.this[each.key].id
  access_key_id = garage_key.tenant[each.value.tenant].id
  read          = true
  write         = true
}

resource "garage_key" "owner" {
  name = "opentofu"
}

resource "garage_bucket_permission" "owner" {
  for_each      = local.buckets
  bucket_id     = garage_bucket.this[each.key].id
  access_key_id = garage_key.owner.id
  owner         = true
}

resource "terraform_data" "lifecycle" {
  for_each         = local.buckets
  triggers_replace = [garage_bucket.this[each.key].id, local.lifecycle[each.key]]
  depends_on       = [garage_bucket_permission.owner]

  provisioner "local-exec" {
    command = "aws --endpoint-url \"$S3_URL\" --region \"$S3_REGION\" s3api put-bucket-lifecycle-configuration --bucket \"$BUCKET\" --lifecycle-configuration \"$RULES\""
    environment = {
      S3_URL                = var.s3_url
      S3_REGION             = var.s3_region
      BUCKET                = each.key
      RULES                 = local.lifecycle[each.key]
      AWS_ACCESS_KEY_ID     = garage_key.owner.id
      AWS_SECRET_ACCESS_KEY = garage_key.owner.secret_access_key
      # A session token of the operator's own AWS login would be sent along.
      AWS_SESSION_TOKEN = ""
    }
  }
}
