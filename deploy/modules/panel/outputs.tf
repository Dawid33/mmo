output "namespace" {
  value = kubernetes_namespace_v1.panel.metadata[0].name
}

output "world_binding" {
  description = "What a world module's `panel` variable takes: the ServiceAccount to bind ClusterRole world-panel to in the world's namespace."
  value = {
    namespace       = kubernetes_namespace_v1.panel.metadata[0].name
    service_account = kubernetes_service_account_v1.panel.metadata[0].name
    cluster_role    = kubernetes_cluster_role_v1.world_panel.metadata[0].name
  }
}
