# Shared platform of the orion k3s server: cert-manager now; CNPG etc. later.
# Apply before ../monitoring (its Grafana Ingress needs the ClusterIssuer).
#   just orion-init platform && just orion-plan platform && just orion-apply platform
module "cert_manager" {
  source = "../../../modules/cert-manager"
}

output "issuer_name" {
  value = module.cert_manager.issuer_name
}
