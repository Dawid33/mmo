# The orion cluster's container registry: private ECR repositories (mmo/login
# mmo/server and mmo/web for the game worlds, mmo/panel for the world panel; one more image is one more entry
# in `repositories`; `repository_urls` carries all of them) and
# the pull-only IAM user `orion-ecr-pull` the cluster pulls with.
#
# Applied BEFORE the login root: that root reads this state (repository URL,
# registry host, the pull user's key), and its first pod needs an image here.
#   just orion-init registry && just orion-plan registry && just orion-apply registry
#   just orion-login-push   # pushes <repo>:latest (the only tag)
#   just orion-apply login
module "registry" {
  source = "../../../modules/registry"
}

output "repository_url" {
  description = "The login service's repository (scripts/orion_login_push.sh reads it)."
  value       = module.registry.repository_urls["mmo/login"]
}

output "repository_urls" {
  value = module.registry.repository_urls
}

output "registry_host" {
  value = module.registry.registry_host
}

output "region" {
  value = module.registry.region
}

output "pull_access_key_id" {
  value = module.registry.pull_access_key_id
}

output "pull_secret_access_key" {
  value     = module.registry.pull_secret_access_key
  sensitive = true
}
