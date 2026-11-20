# One game world on Kubernetes (plan docs/superpowers/plans/2026-11-15-r5-k8s.md,
# Task 11; hosts as Deployments: 2026-11-15-elastic-1-hosts-as-deployments.md; hosts
# made by a reconciler: 2026-11-19-elastic-4-reconciler-scaling.md): a coordinator
# (coordinator.tf), the reconciler `fleet` (fleet.tf) that makes one Deployment,
# NodePort Service and PodDisruptionBudget per row of the table desired_hosts from the
# template in the ConfigMap host-template (hosts.tf), a toolbox pod for worldctl
# (below), the browser bundle (web.tf), the coordinator's service-link identities
# (identities.tf), and the ServiceMonitor and client Certificate through
# charts/world-monitor (below). OpenTofu does not create, scale or delete hosts.
#
# Images: the workloads are created with var.images and then ignore their image;
# releases set digests with `kubectl set image` (scripts/world_update.sh: hosts
# stop first, then the images change, then the hosts come back). The object names
# that script uses are fixed here: Deployments coordinator / toolbox / web / fleet with
# containers of the same names; Deployments host-<name> (one per desired_hosts row,
# labels app.kubernetes.io/component=host, errantry/host=<name>,
# errantry/fleet=<world id>) with container host, created on the coordinator's image.
#
# The coordinator and the fleet are created without waiting for a rollout: they stay
# unready until the world is forged (the coordinator: VOXEL_EMPTY_STORE=wait), which is
# done afterwards from the toolbox
# (`kubectl exec deploy/toolbox -- sh -c 'worldctl forge --db "$VOXEL_WORLD_DB" …'`).
# Then the coordinator wants its first hosts (fleet.min) and the reconciler makes them.
#
# Stopping a world: scale every host Deployment to 0 first (each asks for a leave
# cut, ≤ ~90 s), then the coordinator, as world_update.sh does. A plain `kubectl delete
# namespace` stops everything at once and is not a supported way to stop it.
#
# Files and the server's uid: the server image runs as uid/gid 10001. Every pod
# of the server image runs as 10001 with fsGroup 10001, so mounted Secrets
# (mode 0440: owner root, group 10001) are readable, and the peer identity's
# emptyDir is writable.

locals {
  ns = kubernetes_namespace_v1.world.metadata[0].name
  labels = {
    "app.kubernetes.io/name"     = "world"
    "app.kubernetes.io/instance" = var.world_id
  }
  # A workload's pod labels (and its Services' selectors).
  coordinator_labels = merge(local.labels, { "app.kubernetes.io/component" = "coordinator" })
  host_labels        = merge(local.labels, { "app.kubernetes.io/component" = "host" })
  toolbox_labels     = merge(local.labels, { "app.kubernetes.io/component" = "toolbox" })
  web_labels         = merge(local.labels, { "app.kubernetes.io/component" = "web" })

  # The one definition of the ownership mode: coordinator and hosts must agree.
  ownership_env = { VOXEL_OWNERSHIP = var.ownership }

  uid            = 10001
  secret_mode    = "0440"
  health_port    = 9100
  coordinator_fq = "coordinator.${local.ns}.svc.cluster.local"
  # The coordinator's plain-HTTP world map tile route (crates/server/src/map_api.rs), the web proxy's
  # upstream for /map/<world>/.
  map_port = 6470

  # Mount points (the hash files' directories hold the identities' Secrets).
  service_token_dir = "/run/voxel/service"
  coordinator_dir   = "/run/voxel/coordinator"
  peer_dir          = "/run/voxel/peer"
  client_cert_dir   = "/run/voxel/client"

  # Env both the coordinator and the hosts get (plain values; secrets are refs).
  # VOXEL_WORLD_ID: every link hello names the world and a listener refuses another
  # world's; it is also the world's id in the login registry (`world add <id>`).
  server_env = merge(local.ownership_env, {
    VOXEL_WORLD_ID                   = var.world_id
    VOXEL_SERVICE_TOKEN_FILE         = "${local.service_token_dir}/service-token"
    VOXEL_COORDINATOR_CERT_HASH_FILE = "${local.coordinator_dir}/${local.link_file}"
    VOXEL_EMPTY_STORE                = "wait"
    VOXEL_SAVE_SECS                  = tostring(var.save_secs)
    VOXEL_SUPER_REGION_REGIONS       = tostring(var.super_region_regions)
    VOXEL_HEALTH_BIND                = "0.0.0.0:${local.health_port}"
  })

  # A changed secret rolls the pods that read it (env and files are read at start):
  # the coordinator its identity, the hosts only the coordinator's
  # certificate. A changed hosts_checksum changes the host template (hosts.tf): the
  # reconciler re-templates each host Deployment at its next stop, no running host
  # restarts (the pods read the Secrets by name at their start in any case). Not the
  # pull secret: the kubelet reads that.
  hosts_checksum = sha256(join(",", [
    var.database_url,
    var.service_token,
    tls_self_signed_cert.coordinator.cert_pem,
  ]))
  coordinator_checksum = local.hosts_checksum

  # Workload resources: request cpu and memory, limit memory only.
  coordinator_resources = {
    requests = { cpu = var.resources.coordinator.cpu, memory = var.resources.coordinator.memory }
    limits   = { memory = var.resources.coordinator.memory }
  }
  host_resources = {
    requests = { cpu = var.resources.host.cpu, memory = var.resources.host.memory }
    limits   = { memory = var.resources.host.memory }
  }
}

resource "kubernetes_namespace_v1" "world" {
  metadata {
    name   = coalesce(var.namespace, "world-${var.world_id}")
    labels = local.labels
  }
}

# The database URL reaches pods only as this Secret's key `url` (env VOXEL_WORLD_DB).
resource "kubernetes_secret_v1" "db" {
  metadata {
    name      = "world-db"
    namespace = local.ns
    labels    = local.labels
  }
  data = {
    url = var.database_url
  }
}

# The login service's token, mounted as a file (VOXEL_SERVICE_TOKEN_FILE).
resource "kubernetes_secret_v1" "service" {
  metadata {
    name      = "world-service"
    namespace = local.ns
    labels    = local.labels
  }
  data = {
    "service-token" = var.service_token
  }
}

# Where worldctl runs (forge, clock-reset, super-region, retire-host, rollback):
# `kubectl exec deploy/toolbox -- sh -c 'worldctl … "$VOXEL_WORLD_DB" …'`, so the
# URL never appears on a command line outside the pod.
resource "kubernetes_deployment_v1" "toolbox" {
  metadata {
    name      = "toolbox"
    namespace = local.ns
    labels    = local.toolbox_labels
  }
  spec {
    replicas = 1
    strategy {
      type = "Recreate"
    }
    selector {
      match_labels = local.toolbox_labels
    }
    template {
      metadata {
        labels = local.toolbox_labels
        annotations = {
          "checksum/secrets" = sha256(var.database_url)
        }
      }
      spec {
        automount_service_account_token = false
        enable_service_links            = false
        # `sleep` as PID 1 ignores SIGTERM; nothing to drain.
        termination_grace_period_seconds = 1
        dynamic "image_pull_secrets" {
          for_each = var.image_pull_secret == null ? [] : [var.image_pull_secret]
          content {
            name = image_pull_secrets.value
          }
        }
        security_context {
          run_as_user     = local.uid
          run_as_group    = local.uid
          run_as_non_root = true
          fs_group        = local.uid
        }
        container {
          name              = "toolbox"
          image             = var.images.server
          image_pull_policy = "IfNotPresent"
          command           = ["sleep", "infinity"]
          env {
            name = "VOXEL_WORLD_DB"
            value_from {
              secret_key_ref {
                name = kubernetes_secret_v1.db.metadata[0].name
                key  = "url"
              }
            }
          }
          security_context {
            allow_privilege_escalation = false
            capabilities {
              drop = ["ALL"]
            }
          }
          resources {
            requests = { cpu = "10m", memory = "64Mi" }
            limits   = { memory = var.resources.toolbox.memory }
          }
        }
      }
    }
  }
  # Releases set the image (world_update.sh); an apply must not undo them.
  lifecycle {
    ignore_changes = [spec[0].template[0].spec[0].container[0].image]
  }
}

# Custom resources (a ServiceMonitor, a cert-manager Certificate) go through a
# small local chart. Not released at all when neither is wanted, so a cluster
# without cert-manager or the Prometheus operator needs neither.
resource "helm_release" "monitor" {
  count     = var.metrics || var.client_certificate != null ? 1 : 0
  name      = "world-monitor"
  namespace = local.ns
  chart     = "${path.module}/charts/world-monitor"

  values = [yamlencode({
    metrics           = var.metrics
    selectorLabels    = local.labels
    publicHost        = var.public_host
    clientCertificate = var.client_certificate == null ? {} : { issuer = var.client_certificate.issuer }
  })]

  depends_on = [kubernetes_service_v1.coordinator, kubernetes_service_v1.hosts, kubernetes_service_v1.fleet]
}
