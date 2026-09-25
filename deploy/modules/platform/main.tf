# Cluster-wide operators the world depends on. Applied as its own root BEFORE
# `world`: the world creates Agones and CloudNativePG custom resources, and
# `kubernetes_manifest` needs their CRDs to exist at plan time.

locals {
  replicas = var.single_node ? 1 : 2
}

resource "kubernetes_namespace_v1" "game" {
  metadata {
    name = var.game_namespace
  }
}

resource "helm_release" "local_storage" {
  count            = var.install_local_storage ? 1 : 0
  name             = "localpv"
  repository       = "https://openebs.github.io/dynamic-localpv-provisioner"
  chart            = "localpv-provisioner"
  version          = var.localpv_chart_version
  namespace        = "openebs"
  create_namespace = true
  wait             = true
  values = [yamlencode({
    hostpathClass = { isDefaultClass = true }
    analytics     = { enabled = false }
  })]
}

resource "helm_release" "cnpg" {
  name             = "cnpg"
  repository       = "https://cloudnative-pg.github.io/charts"
  chart            = "cloudnative-pg"
  version          = var.cnpg_chart_version
  namespace        = "cnpg-system"
  create_namespace = true
  wait             = true
  values = [yamlencode({
    replicaCount = local.replicas
  })]
}

resource "helm_release" "agones" {
  name             = "agones"
  repository       = "https://agones.dev/chart/stable"
  chart            = "agones"
  version          = var.agones_version
  namespace        = "agones-system"
  create_namespace = true
  wait             = true
  timeout          = 600
  values = [yamlencode({
    gameservers = {
      namespaces = [kubernetes_namespace_v1.game.metadata[0].name]
      minPort    = var.agones_port_range.min
      maxPort    = var.agones_port_range.max
    }
    agones = {
      controller = { replicas = local.replicas }
      extensions = { replicas = local.replicas }
      # Clients dial game server pods directly (node IP + host port); the
      # allocator is never exposed, and the latency-ping service is unused.
      # Both default to LoadBalancer Services, which never get an address on
      # a bare-metal k0s and hang the install.
      ping = { install = false }
      allocator = {
        replicas  = local.replicas
        service   = { serviceType = "ClusterIP" }
        processor = { replicas = local.replicas }
      }
    }
  })]
}
