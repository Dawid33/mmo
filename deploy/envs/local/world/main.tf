# The twin's game world `twin` (namespace world-twin): the live world module with two
# hosts (a, b) on the same node ports as orion, imported images (mmo/server:twin,
# mmo/web:twin; no pull secret), each host's self-signed client certificate (pinned by
# clients: the other certificate path than orion's Let's Encrypt one), plain-HTTP web
# at http://game.localhost:8480/, no ServiceMonitor, a 10 s save period and small
# requests. Needs ../postgres and ../login applied (their local state is read here).
# Forge it from the toolbox: `just world_ctx=k3d-mmo world_ns=world-twin world-ctl forge`.
# An apply owns the replica counts (coordinator: the module's coordinator_replicas, 2;
# each host Deployment 1: applied to a
# stopped world it starts it). An apply that changes anything the coordinator and the
# hosts share (database_url, service_token, the coordinator's certificate, save_secs,
# super_region_regions, ownership) restarts them all at once, the hosts likely leave
# without a cut and the next start raises the world generation: make such a change with
# the world stopped (`just world_ctx=k3d-mmo world_ns=world-twin world-stop`, then
# apply). Only a host-only change (a key of `hosts`, a host's resources) restarts hosts
# through their cuts on a running world with no update under way. A host added after a
# release starts on the tag, not the released digest: see ../../orion/world/main.tf.
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

  hosts = {
    a = { quic_node_port = 30466, wt_node_port = 30467 }
    b = { quic_node_port = 30468, wt_node_port = 30469 }
  }
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
    toolbox     = { memory = "1Gi" }
  }
}

output "login_link" {
  value = module.world.login_link
}

output "login_link_cert_pem" {
  value = module.world.login_link_cert_pem
}

output "endpoints" {
  value = module.world.endpoints
}

output "host_names" {
  value = module.world.host_names
}
