# cert-manager + a Let's Encrypt ClusterIssuer (HTTP-01 through the cluster's
# ingress controller). Shared platform: Grafana's Ingress uses it now, the game's
# WebTransport certificate later.

resource "helm_release" "cert_manager" {
  name             = "cert-manager"
  namespace        = "cert-manager"
  create_namespace = true
  repository       = "https://charts.jetstack.io"
  chart            = "cert-manager"
  version          = var.cert_manager_version
  wait             = true
  timeout          = 600

  values = [yamlencode({
    crds = { enabled = true, keep = true }
    # k3s: kube-system is fine for the lease (the chart's default).
    resources  = { requests = { cpu = "10m", memory = "64Mi" }, limits = { memory = "256Mi" } }
    webhook    = { resources = { requests = { cpu = "10m", memory = "32Mi" }, limits = { memory = "128Mi" } } }
    cainjector = { resources = { requests = { cpu = "10m", memory = "64Mi" }, limits = { memory = "256Mi" } } }
    prometheus = { enabled = true, servicemonitor = { enabled = false } }
  })]
}

# The ClusterIssuer is a custom resource: shipped as a tiny local chart so `tofu plan`
# never needs cert-manager's CRDs to exist (kubernetes_manifest would).
resource "helm_release" "cluster_issuer" {
  name      = "cluster-issuer"
  namespace = "cert-manager"
  chart     = "${path.module}/charts/cluster-issuer"

  values = [yamlencode({
    name         = var.issuer_name
    server       = var.acme_server
    email        = var.acme_email
    ingressClass = var.ingress_class
  })]

  depends_on = [helm_release.cert_manager]
}
