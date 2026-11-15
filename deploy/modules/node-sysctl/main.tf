# Kernel settings of the cluster's nodes, set from inside the cluster: a
# DaemonSet whose privileged init container writes each sysctl, then idles in a
# pause container. Nothing else in deploy/ manages the host, so this is where
# a node setting is written down; a reboot or a new node gets it again when the
# pod starts. A custom chart because the platform root has only the helm
# provider (like traefik-config).
#
# Check with:
#   just orion-kube -n kube-system logs ds/node-sysctl -c sysctl

resource "helm_release" "node_sysctl" {
  name      = "node-sysctl"
  namespace = "kube-system"
  chart     = "${path.module}/charts/node-sysctl"

  values = [yamlencode({
    sysctls = var.sysctls
    images  = var.images
  })]
}
