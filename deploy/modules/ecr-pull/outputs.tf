output "secret_name" {
  description = "The pull secret's name. Taken from the resource, so a consumer that lists it in imagePullSecrets is created after it exists."
  value       = kubernetes_secret_v1.ecr_pull.metadata[0].name
}
