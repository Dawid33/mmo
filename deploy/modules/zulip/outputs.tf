output "url" {
  value = "https://${var.host}"
}

output "namespace" {
  value = local.ns
}

output "buckets" {
  value = local.buckets
}
