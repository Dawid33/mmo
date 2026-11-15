# PicoShare (file sharing) on the orion k3s server: https://share.errantry.renownandruin.com.
# Needs ../platform applied first (cert-manager's ClusterIssuer; Traefik's upload timeouts).
#   just orion-init picoshare && just orion-plan picoshare && just orion-apply picoshare
#   just orion-share-password   # the upload passphrase
module "picoshare" {
  source = "../../../modules/picoshare"
}

output "url" {
  value = module.picoshare.url
}

output "shared_secret" {
  value     = module.picoshare.shared_secret
  sensitive = true
}
