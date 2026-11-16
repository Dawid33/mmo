output "namespace" {
  value = local.ns
}

output "login_link" {
  description = "The coordinator's login link, for the login registry (`login world add <world_id> … --link`). Resolved by name at every dial."
  value       = "${kubernetes_service_v1.coordinator.metadata[0].name}.${local.ns}.svc.cluster.local:6490"
}

output "login_link_cert_pem" {
  description = "The login link's public certificate. Its pin (`--link-cert`) is the SHA-256 of the DER: `openssl x509 -outform der | sha256sum`."
  value       = tls_self_signed_cert.login_link.cert_pem
}

output "endpoints" {
  description = "Per host name: what clients are told (VOXEL_ADVERTISE_QUIC / _WT)."
  value = {
    for k, h in var.hosts : k => {
      quic = "${var.public_host}:${h.quic_node_port}"
      wt   = "https://${var.public_host}:${h.wt_node_port}/"
    }
  }
}

output "host_names" {
  description = "The hosts' names (var.hosts' keys); each runs as Deployment host-<name>."
  value       = keys(var.hosts)
}
