# Local dev cluster: operators + game namespace. Apply before ../world.
# State is local (gitignored): this cluster is disposable.
module "platform" {
  source      = "../../../modules/platform"
  single_node = true
}
