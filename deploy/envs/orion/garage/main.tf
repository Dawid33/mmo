# Garage, the S3-compatible object store of the orion k3s server:
# https://s3.errantry.renownandruin.com (S3 API; every request needs a key) and
# https://<bucket>.s3web.errantry.renownandruin.com (website buckets, read-only).
# Needs ../platform (cert-manager's ClusterIssuer) and ../monitoring (the
# ServiceMonitor CRD) applied first. Buckets and keys: ../garage-buckets.
#   just orion-init garage && just orion-plan garage && just orion-apply garage
variable "metrics" {
  description = "Garage's ServiceMonitor. Its CRD comes with ../monitoring, which itself needs Garage: on a fresh server apply this root once with `-var metrics=false`, then again after monitoring."
  type        = bool
  default     = true
}

module "garage" {
  source  = "../../../modules/garage"
  metrics = var.metrics
}

output "s3_region" {
  value = module.garage.s3_region
}

output "s3_endpoint" {
  value = module.garage.s3_endpoint
}

output "s3_url" {
  value = module.garage.s3_url
}

output "website_urls" {
  value = module.garage.website_urls
}

output "admin_url" {
  value = module.garage.admin_url
}

output "admin_token" {
  value     = module.garage.admin_token
  sensitive = true
}
