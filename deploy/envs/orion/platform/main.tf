# Shared platform of the orion k3s server: cert-manager, Traefik's settings
# and the node's sysctls now; CNPG etc. later. Apply before ../monitoring and ../picoshare (their
# Ingresses need the ClusterIssuer; PicoShare's uploads need the read timeout).
#   just orion-init platform && just orion-plan platform && just orion-apply platform
module "cert_manager" {
  source = "../../../modules/cert-manager"
}

# No 60 s cap on request bodies: long uploads (../picoshare) go through.
module "traefik_config" {
  source = "../../../modules/traefik-config"
}

# Node kernel settings (inotify instances: the default 128 ran out).
module "node_sysctl" {
  source = "../../../modules/node-sysctl"
}

# The CloudNativePG operator and its CRDs: ../postgres needs them.
module "cnpg" {
  source = "../../../modules/cnpg"
}

output "issuer_name" {
  value = module.cert_manager.issuer_name
}
