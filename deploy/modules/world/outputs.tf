output "namespace" {
  value = local.ns
}

output "login_link" {
  description = "The coordinator's HTTPS API URL, for the login registry (`login world add <world_id> … --link`). Resolved by name at every dial."
  value       = "https://${kubernetes_service_v1.coordinator.metadata[0].name}.${local.ns}.svc.cluster.local:6480"
}

output "coordinator_cert_pem" {
  description = "The coordinator's public certificate (hosts and login pin it). Its pin (`--link-cert`) is the SHA-256 of the DER: `openssl x509 -outform der | sha256sum`."
  value       = tls_self_signed_cert.coordinator.cert_pem
}

output "client_ports" {
  description = "The UDP node ports the hosts' NodePort Services may open (var.fleet), both ends included: slot s is QUIC first + 2s and WebTransport first + 2s + 1. Open the whole range to clients."
  value = {
    first = var.fleet.port_base
    last  = var.fleet.port_base + 2 * var.fleet.max - 1
  }
}
