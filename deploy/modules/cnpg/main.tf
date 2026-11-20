# The CloudNativePG operator and its CRDs (Cluster, Database, ...). Apply this
# BEFORE module.postgres and module.pg-management. The chart is cluster-wide by
# default, so its one operator reconciles `pg` and `pg-management` in their
# separate namespaces.
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
