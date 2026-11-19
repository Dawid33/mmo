# The region hosts (VOXEL_ROLE=host). OpenTofu no longer creates them (Elastic 4, plan
# docs/superpowers/plans/2026-11-19-elastic-4-reconciler-scaling.md): the coordinator's
# decisions write the table desired_hosts (one row per host the world wants: a name h<n>
# the store issues and never reuses, and a slot), and the reconciler (fleet.tf) makes one
# Deployment, NodePort Service and PodDisruptionBudget host-<name> per row from the
# template below, and deletes those of a row that is gone. This file renders that
# template into the ConfigMap host-template (key template.json): a JSON list of the three
# manifests, with the same pod spec as the Deployments this module made before, and
# placeholders the reconciler substitutes (crates/server/src/fleet.rs, Template::render):
#   __NAME__       the row's name (VOXEL_HOST_NAME, label errantry/host, host-<name>)
#   __QUIC_PORT__  port_base + 2·slot   (a JSON string that is ONLY the placeholder,
#   __WT_PORT__    port_base + 2·slot + 1  quotes included, becomes a bare number)
#   __IMAGE__      the coordinator's image when the host is created
# The template holds NO secret value: Secrets are referenced by name (secretKeyRef,
# secret volumes), as the Deployments did. Every object carries errantry/fleet=<world id>
# (what the reconciler lists by) and errantry/host=<name>. The reconciler adds to the
# Deployment spec.replicas (1 at its create) and the annotation errantry/template (the
# sha256 of template.json).
#
# A changed template (an apply that changes anything below: env, resources, the secrets'
# checksum) touches no running host: the reconciler re-templates a host Deployment only
# while it sits at 0 replicas (world-stop) and the coordinator does not, and it never
# changes an existing Deployment's replicas or image. So an apply on a running world
# restarts no host; a host-only change reaches each host at its next stop and start. The
# replica counts and images of host Deployments belong to the recipes (world-stop,
# world-start, scripts/world_update.sh), as before.
#
# A host's identity is the id the store issues when it registers; its name labels the
# process (log lines; a restarted process of the same name lapses its old registration at
# once). Nothing is derived from a number: the addresses clients are told carry the node
# ports of the host's slot, and the peer address is the pod's IP.
#
# Differences from scripts/dev_r3.sh, on purpose: dev restarts a host only on exit
# 5 or 137 (and 3 under claim) and gives up after 3 restarts in 60 s; here every
# exit restarts (restartPolicy Always: 0, 1, 3, 4, 5, 137) and Kubernetes never
# gives up (CrashLoopBackOff, up to 5 min apart) — an alert watches restart loops
# instead. Dev starts hosts after the coordinator's pin file appears; here the pin
# comes from the coordinator's certificate in a Secret and a host dials until the
# coordinator answers.
#
# Each Deployment has 1 replica and strategy Recreate: a template change stops the old
# pod (SIGTERM: it asks for a leave cut, ≤ ~90 s) before the new one starts. Hosts are
# independent Deployments: a boot waiting on a succession (unready, maybe for minutes)
# holds back no other host. No CPU limit: a throttled main loop is a missed lease
# heartbeat.
#
# A new host is created on the image of the coordinator container of the coordinator
# Deployment at that moment (the released digest): the world never runs mixed builds
# because a host was added.
#
# Draining and removing a host: the coordinator drains the hosts it no longer wants
# (DrainHost); `worldctl drain` marks one by hand and the coordinator's row follows the
# mark. At its deadline a host leaves through a leave cut and stays up, unready (/readyz
# 503 `drained`, /livez 200); once every registration of its name is clean or dead the
# coordinator removes the row and the reconciler deletes its Deployment, Service and
# PodDisruptionBudget.
#
# Emptying a node (a kernel update, a move): the PodDisruptionBudget (max_unavailable 0)
# makes `kubectl drain` wait on every host pod of the node. The coordinator's budget
# (min_available 1, coordinator.tf) lets a drain evict all but one of its replicas; the
# toolbox, web and fleet have NO budget: a drain evicts them at once. On ONE node the
# evicted replicas cannot reschedule and the last one blocks the drain, the toolbox goes,
# so the hosts would run with one coordinator replica and no worldctl and could not even
# be drained (that needs the coordinator and the toolbox): `just world-stop` FIRST, then
# drain the node (a host Deployment at 0 replicas has no pod to block it). With several
# nodes: drain the node's hosts (`just world-drain <name>`) before `kubectl drain`. The
# budget is an operations guard against an eviction that would turn a host into a crash;
# it is not what keeps two writers apart (the store's fencing is).

locals {
  # A host's pods' labels (the Deployment's selector); the Service and the budget select a
  # subset. The pods do not carry errantry/fleet: only the three objects do.
  host_pod_labels    = merge(local.host_labels, { "errantry/host" = "__NAME__" })
  host_object_labels = merge(local.host_pod_labels, { "errantry/fleet" = var.world_id })

  # Kubernetes takes a volume's mode as a number; local.secret_mode is octal text.
  secret_mode_number = parseint(local.secret_mode, 8)

  # The plain env of a host (sorted by name below, as the dynamic block over a map did).
  host_env = merge(local.server_env, {
    VOXEL_ROLE                = "host"
    VOXEL_HOST_NAME           = "__NAME__"
    VOXEL_ADVERTISE_QUIC      = "${var.public_host}:__QUIC_PORT__"
    VOXEL_ADVERTISE_WT        = "https://${var.public_host}:__WT_PORT__/"
    VOXEL_COORDINATOR_ADDR    = "https://${local.coordinator_fq}:6480"
    VOXEL_BIND_ADDR           = "0.0.0.0"
    VOXEL_SERVER_PORT         = "6466"
    VOXEL_WT_PORT             = "6467"
    VOXEL_PEER_BIND           = "0.0.0.0:6500"
    VOXEL_PEER_CERT_HASH_FILE = "${local.peer_dir}/peer.cert-sha256"
    VOXEL_AUTH                = "jwt"
    VOXEL_LOGIN_URL           = var.auth.login_url
    }, var.client_certificate == null ? {} : {
    VOXEL_CLIENT_CERT = "${local.client_cert_dir}/tls.crt"
    VOXEL_CLIENT_KEY  = "${local.client_cert_dir}/tls.key"
    }, var.land_idle_ms == null ? {} : {
    VOXEL_LAND_IDLE_MS = tostring(var.land_idle_ms)
  })

  # cert-manager's Secret (charts/world-monitor): the pod starts once it exists.
  host_client_tls = var.client_certificate == null ? [] : ["client-tls"]

  host_deployment = {
    apiVersion = "apps/v1"
    kind       = "Deployment"
    metadata = {
      name   = "host-__NAME__"
      labels = local.host_object_labels
    }
    spec = {
      strategy = { type = "Recreate" }
      selector = { matchLabels = local.host_pod_labels }
      template = {
        metadata = {
          labels = local.host_pod_labels
          # A hash, not a secret: a changed secret re-templates a host at its next stop.
          annotations = { "checksum/secrets" = local.hosts_checksum }
        }
        spec = {
          automountServiceAccountToken = false
          enableServiceLinks           = false
          # A leave cut is bounded by about 90 s (dev_r3.sh allows 95).
          terminationGracePeriodSeconds = 100
          imagePullSecrets = [
            for s in(var.image_pull_secret == null ? [] : [var.image_pull_secret]) : { name = s }
          ]
          securityContext = {
            runAsUser    = local.uid
            runAsGroup   = local.uid
            runAsNonRoot = true
            fsGroup      = local.uid
          }
          volumes = concat([
            {
              name = "service"
              secret = {
                secretName  = kubernetes_secret_v1.service.metadata[0].name
                defaultMode = local.secret_mode_number
              }
            },
            # Only the coordinator's certificate: the host derives the pin from it.
            {
              name = "coordinator-cert"
              secret = {
                secretName  = kubernetes_secret_v1.link_identity["coordinator"].metadata[0].name
                defaultMode = local.secret_mode_number
                items       = [{ key = "${local.link_file}.cert", path = "${local.link_file}.cert" }]
              }
            },
            # The peer identity: made at each pod's first start and published in the
            # `hosts` table at registration (a new pin per pod).
            {
              name     = "peer"
              emptyDir = { medium = "Memory", sizeLimit = "1Mi" }
            },
            ], [
            for v in local.host_client_tls : {
              name   = v
              secret = { secretName = v, defaultMode = local.secret_mode_number }
            }
          ])
          containers = [{
            name            = "host"
            image           = "__IMAGE__"
            imagePullPolicy = "IfNotPresent"
            # The image's CMD `server` is PID 1, so SIGTERM reaches it: an exiting host
            # asks for a leave cut.
            # ORDER MATTERS: $(POD_IP) below expands only because POD_IP is declared first.
            env = concat([
              { name = "POD_IP", valueFrom = { fieldRef = { fieldPath = "status.podIP" } } },
              { name = "VOXEL_ADVERTISE_PEER", value = "$(POD_IP):6500" },
              ], [
              for k in sort(keys(local.host_env)) : { name = k, value = local.host_env[k] }
              ], [
              { name = "VOXEL_WORLD_DB", valueFrom = { secretKeyRef = { name = kubernetes_secret_v1.db.metadata[0].name, key = "url" } } },
            ])
            ports = [
              { name = "quic", containerPort = 6466, protocol = "UDP" },
              { name = "wt", containerPort = 6467, protocol = "UDP" },
              { name = "peer", containerPort = 6500, protocol = "UDP" },
              { name = "health", containerPort = local.health_port },
            ]
            volumeMounts = concat([
              { name = "service", mountPath = local.service_token_dir, readOnly = true },
              { name = "coordinator-cert", mountPath = local.coordinator_dir, readOnly = true },
              { name = "peer", mountPath = local.peer_dir },
              ], [
              for v in local.host_client_tls : { name = v, mountPath = local.client_cert_dir, readOnly = true }
            ])
            securityContext = {
              allowPrivilegeEscalation = false
              capabilities             = { drop = ["ALL"] }
            }
            resources = {
              requests = { for k, v in local.host_resources.requests : k => tostring(v) }
              limits   = { for k, v in local.host_resources.limits : k => tostring(v) }
            }
            # No startup probe: /livez is 200 until the process has registered, so a long
            # boot (a forge wait, a succession wait) never fails it. It fails only after
            # LIVEZ_STALL_MS (30 s) of a stalled registered loop, so this fires after about
            # a minute — long after the lease (10 s) has handed the host's land to an heir.
            # A host waiting for its forge or its succession, or drained, is live and unready.
            livenessProbe = {
              httpGet          = { path = "/livez", port = "health" }
              periodSeconds    = 10
              failureThreshold = 3
            }
            readinessProbe = {
              httpGet       = { path = "/readyz", port = "health" }
              periodSeconds = 5
            }
          }]
        }
      }
    }
  }

  # What clients dial: one NodePort Service per host, selecting exactly its pod.
  # externalTrafficPolicy Local keeps the client's source address (no SNAT).
  host_service = {
    apiVersion = "v1"
    kind       = "Service"
    metadata = {
      name = "host-__NAME__"
      # Not the hosts' component label: the ServiceMonitor must not select these.
      labels = merge(local.labels, {
        "app.kubernetes.io/component" = "host-endpoint"
        "errantry/host"               = "__NAME__"
        "errantry/fleet"              = var.world_id
      })
    }
    spec = {
      type                  = "NodePort"
      externalTrafficPolicy = "Local"
      selector = {
        "app.kubernetes.io/component" = "host"
        "errantry/host"               = "__NAME__"
      }
      ports = [
        { name = "quic", port = 6466, targetPort = "quic", nodePort = "__QUIC_PORT__", protocol = "UDP" },
        { name = "wt", port = 6467, targetPort = "wt", nodePort = "__WT_PORT__", protocol = "UDP" },
      ]
    }
  }

  # A voluntary eviction (a node drain) of a host's pod waits: see the header.
  host_pdb = {
    apiVersion = "policy/v1"
    kind       = "PodDisruptionBudget"
    metadata = {
      name   = "host-__NAME__"
      labels = local.host_object_labels
    }
    spec = {
      maxUnavailable = 0
      selector       = { matchLabels = { "errantry/host" = "__NAME__" } }
    }
  }
}

# The reconciler's template (fleet.tf reads it; a changed byte is a new template hash).
resource "kubernetes_config_map_v1" "host_template" {
  metadata {
    name      = "host-template"
    namespace = local.ns
    labels    = local.labels
  }
  data = {
    "template.json" = jsonencode([local.host_deployment, local.host_service, local.host_pdb])
  }
}

# Headless, for the ServiceMonitor only (charts/world-monitor): every host pod is its
# own /metrics target, also while starting or drained (not Ready). Peers dial each
# other's pod IPs from the `hosts` table and the login service dials the coordinator,
# so no other port is here.
resource "kubernetes_service_v1" "hosts" {
  metadata {
    name      = "hosts"
    namespace = local.ns
    labels    = local.host_labels
  }
  spec {
    cluster_ip                  = "None"
    publish_not_ready_addresses = true
    selector                    = local.host_labels
    port {
      name        = "health"
      port        = local.health_port
      target_port = "health"
    }
  }
}
