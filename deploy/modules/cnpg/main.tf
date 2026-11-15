# The CloudNativePG operator and its CRDs (Cluster, Database, ...). Apply this
# BEFORE module.postgres: the pg-cluster chart creates objects of those kinds.
resource "helm_release" "cnpg" {
  name             = "cnpg"
  repository       = "https://cloudnative-pg.github.io/charts"
  chart            = "cloudnative-pg"
  version          = var.chart_version
  namespace        = "cnpg-system"
  create_namespace = true
  wait             = true
  values = [yamlencode({
    replicaCount = var.replicas
  })]
}
