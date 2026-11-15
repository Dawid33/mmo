# Settings for the Traefik k3s ships, as a HelmChartConfig (k3s merges it into
# its own chart and rolls Traefik — a few seconds' blip on every Ingress).
# A custom resource, so it is a tiny local chart like cert-manager's
# ClusterIssuer (the platform root has only the helm provider).
# Passed as static arguments, not chart values, so a k3s chart bump cannot
# rename them away; check with:
#   just orion-kube -n kube-system get deploy traefik -o jsonpath='{.spec.template.spec.containers[0].args}'

resource "helm_release" "traefik_config" {
  name      = "traefik-config"
  namespace = "kube-system"
  chart     = "${path.module}/charts/traefik-config"

  values = [yamlencode({
    arguments = [
      for e in var.entrypoints :
      "--entryPoints.${e}.transport.respondingTimeouts.readTimeout=${var.read_timeout}"
    ]
  })]
}
