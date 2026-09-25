# Local dev world. `game_image` stays null until the server has a container
# image and a configurable bind address (multi-instance stage 3).
variable "game_image" {
  type    = string
  default = null
}

module "world" {
  source          = "../../../modules/world"
  db_storage_size = "2Gi"
  game_image      = var.game_image
  game_resources  = { cpu = "1", memory = "2Gi" }
}

output "database_secret" {
  value = module.world.database_secret
}
