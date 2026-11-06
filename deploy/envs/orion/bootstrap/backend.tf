# Partial backend: bucket / region / encrypt / lock come from ../backend.s3.tfbackend
# (`just orion-init bootstrap` passes it with this root's key).
terraform {
  backend "s3" {}
}
