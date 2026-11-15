# Grafana dashboards as code: every dashboards/*.json becomes a ConfigMap the
# kube-prometheus-stack Grafana sidecar loads (it watches label grafana_dashboard=1
# in every namespace). See dashboards/README.md for where each one comes from.

resource "kubernetes_config_map_v1" "dashboard" {
  for_each = fileset("${path.module}/dashboards", "*.json")

  metadata {
    name      = "dashboard-${trimsuffix(each.value, ".json")}"
    namespace = local.ns
    labels = {
      grafana_dashboard = "1"
    }
  }

  data = {
    (each.value) = file("${path.module}/dashboards/${each.value}")
  }
}
