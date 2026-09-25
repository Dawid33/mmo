# The world: its Postgres database and (once an image exists) the game
# server fleet. Needs the platform root applied first (CRDs, namespace).

locals {
  db_name = "mmo-db"
}

# CloudNativePG cluster. The operator generates the `<name>-app` secret whose
# `uri` key is a ready-made DATABASE_URL, so no password ever enters state.
resource "kubernetes_manifest" "db" {
  manifest = {
    apiVersion = "postgresql.cnpg.io/v1"
    kind       = "Cluster"
    metadata   = { name = local.db_name, namespace = var.namespace }
    spec = {
      instances = var.db_instances
      storage   = { size = var.db_storage_size }
      bootstrap = { initdb = { database = "mmo", owner = "mmo" } }
    }
  }
  wait {
    condition {
      type   = "Ready"
      status = "True"
    }
  }
  timeouts {
    create = "10m"
  }
}

# Game servers. Each pod gets two Dynamic UDP host ports (QUIC + WebTransport)
# on the node's address — the path verified on k0s-in-docker 2026-09-22.
resource "kubernetes_manifest" "game" {
  count = var.game_image == null ? 0 : 1
  manifest = {
    apiVersion = "agones.dev/v1"
    kind       = "Fleet"
    metadata   = { name = "world", namespace = var.namespace }
    spec = {
      replicas = var.game_replicas
      template = {
        spec = {
          # Required once the pod has more than one container (the SDK
          # stand-in below): which one is the game server.
          container = "server"
          ports = [
            { name = "quic", portPolicy = "Dynamic", containerPort = 6466, protocol = "UDP" },
            { name = "wt", portPolicy = "Dynamic", containerPort = 6467, protocol = "UDP" },
          ]
          # Health pings come from the sidecar below until the server speaks
          # the Agones SDK itself.
          health = { initialDelaySeconds = 30, periodSeconds = 5, failureThreshold = 6 }
          template = {
            spec = {
              containers = [
                {
                  name  = "server"
                  image = var.game_image
                  env = concat(
                    [{
                      name      = "DATABASE_URL"
                      valueFrom = { secretKeyRef = { name = "${local.db_name}-app", key = "uri" } }
                    }],
                    [for k, v in var.game_env : { name = k, value = v }],
                  )
                  resources = {
                    requests = { cpu = var.game_resources.cpu, memory = var.game_resources.memory }
                    limits   = { memory = var.game_resources.memory }
                  }
                },
                {
                  # Interim Agones SDK stand-in: mark Ready once, then send
                  # health pings over the SDK sidecar's REST port. Replace with
                  # the Rust SDK in the server (stage 3), which can also tell
                  # the server its allocated host ports.
                  name    = "sdk-ready"
                  image   = "curlimages/curl:8.16.0"
                  command = ["/bin/sh", "-c"]
                  args = [join(" ", [
                    "until curl -sf -X POST -d '{}' -H 'Content-Type: application/json' http://localhost:$${AGONES_SDK_HTTP_PORT}/ready; do sleep 1; done;",
                    "while true; do curl -sf -X POST -d '{}' -H 'Content-Type: application/json' http://localhost:$${AGONES_SDK_HTTP_PORT}/health; sleep 2; done",
                  ])]
                  resources = {
                    requests = { cpu = "5m", memory = "16Mi" }
                    limits   = { memory = "32Mi" }
                  }
                },
              ]
            }
          }
        }
      }
    }
  }
  depends_on = [kubernetes_manifest.db]
}
