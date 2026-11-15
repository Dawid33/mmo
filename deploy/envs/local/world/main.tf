# The twin's game world `twin` (namespace world-twin): the live world module with two
# hosts on the same node ports as orion, imported images (mmo/server:twin,
# mmo/web:twin; no pull secret), each host's self-signed client certificate (pinned by
# clients: the other certificate path than orion's Let's Encrypt one), plain-HTTP web
# at http://game.localhost:8480/, no ServiceMonitor, a 10 s save period and small
# requests. Needs ../postgres and ../login applied (their local state is read here).
# Forge it from the toolbox: `just world_ctx=k3d-mmo world_ns=world-twin world-ctl forge`.
data "terraform_remote_state" "postgres" {
  backend = "local"
  config = {
    path = "../../../../target/twin/postgres.tfstate"
  }
}

data "terraform_remote_state" "login" {
  backend = "local"
  config = {
    path = "../../../../target/twin/login.tfstate"
  }
}

module "world" {
  source = "../../../modules/world"

  world_id = "twin"
  images = {
    server = "mmo/server:twin"
    web    = "mmo/web:twin"
  }
  database_url  = data.terraform_remote_state.postgres.outputs.world_urls["twin"]
  service_token = data.terraform_remote_state.login.outputs.service_token

  hosts = [
    { quic_node_port = 30466, wt_node_port = 30467 },
    { quic_node_port = 30468, wt_node_port = 30469 },
  ]
  public_host        = "127.0.0.1"
  client_certificate = null
  web = {
    ingress_host   = "game.localhost"
    login_upstream = "http://login.login.svc.cluster.local"
  }
  auth      = { login_url = "http://login.login.svc.cluster.local" }
  metrics   = false
  save_secs = 10

  resources = {
    coordinator = { cpu = "50m", memory = "512Mi" }
    host        = { cpu = "100m", memory = "1Gi" }
  }
}

output "host_link" {
  value = module.world.host_link
}

output "endpoints" {
  value = module.world.endpoints
}
