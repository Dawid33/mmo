# Management PostgreSQL: one CloudNativePG cluster and one role/database per
# service. It is deliberately separate from the region cluster's namespace,
# roles, storage, Secrets, backups and state.

locals {
  host = "pg-management-rw.${var.namespace}.svc.cluster.local"
}

resource "kubernetes_namespace_v1" "management" {
  metadata {
    name = var.namespace
  }
}

resource "random_password" "service" {
  for_each = toset(var.services)
  length   = 32
  special  = false
}

resource "kubernetes_secret_v1" "service" {
  for_each = toset(var.services)
  metadata {
    name      = "pg-${each.key}"
    namespace = kubernetes_namespace_v1.management.metadata[0].name
    labels    = { "cnpg.io/reload" = "true" }
  }
  type = "kubernetes.io/basic-auth"
  data = {
    username = each.key
    password = random_password.service[each.key].result
  }
}

resource "helm_release" "cluster" {
  name      = "pg-management"
  namespace = kubernetes_namespace_v1.management.metadata[0].name
  chart     = "${path.module}/charts/pg-management"

  values = [yamlencode({
    services       = var.services
    image          = var.image
    storageClass   = var.storage_class
    storageSize    = var.storage_size
    maxConnections = var.max_connections
    resources      = var.resources
  })]

  depends_on = [kubernetes_secret_v1.service]
}
