# The twin's game world `twin` (namespace world-twin): the live world module with a fleet
# of exactly two hosts (min = max = 2; the coordinator names them h1, h2, … and the
# reconciler makes their Deployments) on the four node ports k3d maps, 30466-30469, as
# orion's, imported images (mmo/server:twin, mmo/web:twin; no pull secret), each host's
# self-signed client certificate (pinned by clients: the other certificate path than
# orion's Let's Encrypt one), plain-HTTP web at http://game.localhost:8480/, no
# ServiceMonitor, a 10 s save period, short scale dials and small requests. Needs
# ../postgres and ../login applied (their local state is read here).
# Forge it from the toolbox: `just world_ctx=k3d-mmo world_ns=world-twin world-ctl forge`.
# An apply owns the coordinator's replica count (the module's coordinator_replicas, 2:
# applied to a stopped world it starts the coordinator, not the hosts — host Deployments
# are the reconciler's and the recipes'). An apply that changes anything the coordinator
# and the hosts share (database_url, service_token, the coordinator's certificate,
# save_secs, super_region_regions, ownership) restarts the coordinator and changes the
# host template: make it with the world stopped (`just world_ctx=k3d-mmo
# world_ns=world-twin world-stop`, apply, then world-start). A change of the host template
# alone (a host's resources, land_idle_ms) restarts no running host: each takes it at its
# next stop and start.
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

  # Idle land goes back after 20 s, not the hosts' 5 min: twin_check phase 7c waits for it.
  land_idle_ms = 20000

  # Two hosts, always: k3d maps exactly the node ports 30466-30469 (two slots). The dials
  # are short so twin_check can watch a drain and a replacement within minutes.
  fleet = {
    min                  = 2
    max                  = 2
    port_base            = 30466
    scale_up_after_ms    = 30000
    scale_down_after_ms  = 30000
    host_min_lifetime_ms = 60000
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

  # k3d resolves the published local-stack ports through its built-in
  # host.k3d.internal gateway. The local profile compose stack is external to
  # the cluster; these endpoints do not couple either root to its state.
  telemetry = {
    otlp_endpoint             = "http://host.k3d.internal:4318"
    coordinator_sampler_ratio = 1
    fleet_sampler_ratio       = 1
    host_sampler_ratio        = 1
    profile_enabled           = true
    profile_endpoint          = "http://host.k3d.internal:4040"
    environment               = "twin"
    cluster                   = "k3d-mmo"
  }

  # The world panel (../panel) may read, start, stop and forge this world. Literal names:
  # the binding does not need the panel root applied first.
  panel = { namespace = "panel", service_account = "panel", cluster_role = "world-panel" }

  resources = {
    coordinator = { cpu = "50m", memory = "512Mi" }
    host        = { cpu = "100m", memory = "1Gi" }
    toolbox     = { memory = "1Gi" }
  }
}

output "login_link" {
  value = module.world.login_link
}

output "coordinator_cert_pem" {
  value = module.world.coordinator_cert_pem
}

output "client_ports" {
  value = module.world.client_ports
}
