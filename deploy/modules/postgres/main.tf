# One CloudNativePG cluster `pg` (single instance) with a database and a login
# role per world. Needs module.cnpg applied first (the CRDs). Hosts connect to
# pg-rw directly: no pooler, so LISTEN/NOTIFY, prepared transactions and the
# fence's SET LOCAL idle_in_transaction_session_timeout behave as in dev.

locals {
  host = "pg-rw.${var.namespace}.svc.cluster.local"
}

resource "kubernetes_namespace_v1" "postgres" {
  metadata {
    name = var.namespace
  }
}

# Changing a password here changes the role's password: the operator reconciles
# managed roles from these Secrets (cnpg.io/reload makes it notice an edit).
resource "random_password" "world" {
  for_each = toset(var.worlds)
  length   = 32
  special  = false
}

resource "kubernetes_secret_v1" "world" {
  for_each = toset(var.worlds)
  metadata {
    name      = "pg-world-${each.key}"
    namespace = kubernetes_namespace_v1.postgres.metadata[0].name
    labels    = { "cnpg.io/reload" = "true" }
  }
  type = "kubernetes.io/basic-auth"
  data = {
    username = "world_${each.key}"
    password = random_password.world[each.key].result
  }
}

# Cluster and Database are custom resources, so a tiny local chart.
resource "helm_release" "cluster" {
  name      = "pg-cluster"
  namespace = kubernetes_namespace_v1.postgres.metadata[0].name
  chart     = "${path.module}/charts/pg-cluster"

  values = [yamlencode({
    worlds                  = var.worlds
    image                   = var.image
    storageClass            = var.storage_class
    storageSize             = var.storage_size
    maxPreparedTransactions = var.max_prepared_transactions
    resources               = var.resources
  })]

  depends_on = [kubernetes_secret_v1.world]
}
