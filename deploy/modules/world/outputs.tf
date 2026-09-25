output "database_secret" {
  description = "Secret holding the app credentials; key `uri` is a DATABASE_URL (in-cluster host)."
  value       = "${local.db_name}-app"
}

output "database_service" {
  value = "${local.db_name}-rw.${var.namespace}.svc"
}
