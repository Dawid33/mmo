# The world panel's access to this world (var.panel): ClusterRole world-panel bound in this
# namespace only (deploy/modules/panel says what it allows).
resource "kubernetes_role_binding_v1" "panel" {
  count = var.panel == null ? 0 : 1
  metadata {
    name      = "world-panel"
    namespace = local.ns
    labels    = local.labels
  }
  role_ref {
    api_group = "rbac.authorization.k8s.io"
    kind      = "ClusterRole"
    name      = var.panel.cluster_role
  }
  subject {
    kind      = "ServiceAccount"
    name      = var.panel.service_account
    namespace = var.panel.namespace
  }
}
