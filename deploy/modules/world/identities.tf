# The coordinator's two service-link listener identities, made here so a restarted
# pod keeps the pin its dialers hold (crates/svc-link/src/cert.rs): `coordinator`
# (hosts dial it on 6480) and `login-link` (the login service dials it on 6490). Each
# is an ECDSA P-256 key and a 10-year self-signed certificate; dialers check only the
# certificate's SHA-256, never its name or dates.
#
# The listener reads `<hash-file>.cert` (certificate DER) and `<hash-file>.key`
# (PKCS#8 DER) beside its VOXEL_*_CERT_HASH_FILE, so with hash file
# /run/voxel/<identity>/link.cert-sha256 the Secret keys are
# link.cert-sha256.cert and link.cert-sha256.key. A Secret's binary_data is the
# base64 of the raw bytes, and a PEM block's body is the base64 of its DER: the
# body with header, footer and newlines removed is the value. The pin file itself
# is not in the Secret: the mount is read-only, the listener only warns that it
# cannot write it, and a dialer that finds no pin file derives it from `.cert`.
#
# Replacing a key changes the pin and rolls the coordinator (main.tf's checksums):
# the coordinator's also rolls every host (they read the pin at start); the login
# link's needs the login registry's --link-cert updated.

locals {
  link_identities = {
    coordinator = { key = tls_private_key.coordinator, cert = tls_self_signed_cert.coordinator }
    login-link  = { key = tls_private_key.login_link, cert = tls_self_signed_cert.login_link }
  }
  # The file name the server derives from its hash file (see the header).
  link_file = "link.cert-sha256"
}

resource "tls_private_key" "coordinator" {
  algorithm   = "ECDSA"
  ecdsa_curve = "P256"
}

resource "tls_self_signed_cert" "coordinator" {
  private_key_pem       = tls_private_key.coordinator.private_key_pem
  validity_period_hours = 24 * 365 * 10
  allowed_uses          = ["digital_signature", "server_auth"]
  dns_names             = ["localhost"]
  subject {
    common_name = "coordinator.${local.ns}"
  }
}

resource "tls_private_key" "login_link" {
  algorithm   = "ECDSA"
  ecdsa_curve = "P256"
}

resource "tls_self_signed_cert" "login_link" {
  private_key_pem       = tls_private_key.login_link.private_key_pem
  validity_period_hours = 24 * 365 * 10
  allowed_uses          = ["digital_signature", "server_auth"]
  dns_names             = ["localhost"]
  subject {
    common_name = "login-link.${local.ns}"
  }
}

resource "kubernetes_secret_v1" "link_identity" {
  for_each = local.link_identities
  metadata {
    name      = "${each.key}-identity"
    namespace = local.ns
    labels    = local.labels
  }
  binary_data = {
    "${local.link_file}.cert" = replace(each.value.cert.cert_pem, "/-----[A-Z ]+-----|\\s/", "")
    "${local.link_file}.key"  = replace(each.value.key.private_key_pem_pkcs8, "/-----[A-Z ]+-----|\\s/", "")
  }
}
