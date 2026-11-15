variable "tenants" {
  description = <<-EOT
    Who stores what. Each tenant gets ONE access key, good for reading and
    writing objects in its own buckets and nothing else. Bucket names are
    global in Garage, so they must be unique across tenants.
      expiry_days  objects are deleted this many days after they were written
                   (null: never). A backstop, not the retention setting.
      website      anyone may read single objects (no listing) at
                   https://<bucket>.<web root domain>; the hostname itself is
                   made in modules/garage (var.website_buckets).
  EOT
  type = map(object({
    buckets = map(object({
      expiry_days = optional(number)
      website     = optional(bool, false)
    }))
  }))
  validation {
    condition     = length(flatten([for t in var.tenants : keys(t.buckets)])) == length(distinct(flatten([for t in var.tenants : keys(t.buckets)])))
    error_message = "Bucket names are global in Garage: two tenants cannot both have a bucket of the same name."
  }
}

variable "s3_url" {
  description = "Garage's S3 API as this machine reaches it (the lifecycle rules are set with the aws CLI)."
  type        = string
}

variable "s3_region" {
  type    = string
  default = "garage"
}

variable "abort_multipart_days" {
  description = "Every bucket: uploads left unfinished this long are swept."
  type        = number
  default     = 2
}
