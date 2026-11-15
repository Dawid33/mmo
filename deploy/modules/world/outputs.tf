output "namespace" {
  value = local.ns
}

output "host_link" {
  description = "The entry host's service link, for the login registry (`login world add … --link`). Resolved by name at every dial."
  value       = "hosts-0.${kubernetes_service_v1.hosts.metadata[0].name}.${local.ns}.svc.cluster.local:6490"
}

output "host_link_cert_pem" {
  description = "The host link's public certificate. Its pin (`--link-cert`) is the SHA-256 of the DER: `openssl x509 -outform der | sha256sum`."
  value       = tls_self_signed_cert.host_link.cert_pem
}

output "endpoints" {
  description = "Per host, in ordinal order: what clients are told (VOXEL_ADVERTISE_QUIC / _WT)."
  value = [
    for h in var.hosts : {
      quic = "${var.public_host}:${h.quic_node_port}"
      wt   = "https://${var.public_host}:${h.wt_node_port}/"
    }
  ]
}
