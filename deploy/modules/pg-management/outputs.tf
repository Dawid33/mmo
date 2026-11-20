output "databases" {
  description = "Service name -> direct read-write connection details."
  sensitive   = true
  value = {
    for service in var.services :
    service => {
      host     = local.host
      port     = 5432
      database = service
      user     = service
      password = random_password.service[service].result
    }
  }
}
