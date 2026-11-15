# The k3d twin of the orion k3s server (`just twin-up`, then `just twin-apply` applies
# platform -> postgres -> login -> world): the CloudNativePG operator only. Orion's
# platform root also has cert-manager and Traefik settings; the twin has neither
# (k3d's bundled Traefik and local-path stay on, as on orion), so its world runs
# with self-signed, pinned client certificates and plain-HTTP Ingresses.
module "cnpg" {
  source = "../../../modules/cnpg"
}
