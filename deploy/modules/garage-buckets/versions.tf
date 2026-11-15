terraform {
  required_version = ">= 1.10"
  required_providers {
    # Community provider (github.com/jkossis/terraform-provider-garage), Garage's
    # admin API v2. Pinned exactly: bump deliberately, against a scratch Garage first.
    garage = { source = "jkossis/garage", version = "1.0.5" }
  }
}
