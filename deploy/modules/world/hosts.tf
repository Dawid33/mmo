# The region hosts (VOXEL_ROLE=host): one Deployment host-<k> per key k of var.hosts,
# each with its own NodePort Service host-<k> and PodDisruptionBudget host-<k>. A host's
# identity is the id the store issues when it registers; the key is only its name
# (VOXEL_HOST_NAME: log lines, and a restarted process of the same name lapses its old
# registration at once) and the label errantry/host. Nothing is derived from a number:
# the addresses clients are told and the peer address are set here per host.
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
# pod (SIGTERM: it asks for a leave cut, ≤ ~90 s) before the new one starts. An apply
# that changes the host pod template therefore restarts EVERY host at once. A change to
# anything the coordinator and the hosts share (database_url, service_token, the
# coordinator's certificate, save_secs, super_region_regions, ownership: local.server_env
# and the checksum annotation) restarts the coordinator in the same moment: the hosts'
# leave cuts then most likely fail, the next start is a total crash (generation raised,
# up to a save interval lost). Make such a change with the world STOPPED (`just
# world-stop`, then apply: the apply sets the replica counts and so starts it). Only a
# host-only change (a key of var.hosts, a host's node ports or resources) restarts hosts
# on a running world, each through its cut (see the roots' headers). Hosts are
# independent Deployments: a boot waiting on a succession (unready, maybe for minutes)
# holds back no other host. No CPU limit: a throttled main loop is a missed lease
# heartbeat.
#
# A host added through var.hosts after a release starts on var.images.server (the tag,
# e.g. :latest) while the rest of the world runs the digest the last release set (images
# are set by digest outside OpenTofu: ignore_changes). Until its image is the
# coordinator's the world runs mixed builds, and the next release finds differing server
# images and records no previous release (no rollback). So: apply, then at once
# `kubectl set image deploy/host-<k> host=<the coordinator's image>`. Between the two the
# new host runs the tag's build for a short time; there is no race-free recipe yet.
#
# Draining and removing a host: `worldctl drain` marks it; at its deadline it leaves
# through a leave cut and stays up, unready (/readyz 503 `drained`, /livez 200), until
# its pod is removed — readiness never restarts a pod, and nothing here waits for a
# rollout. Remove its key from var.hosts and apply: its Deployment, Service and
# PodDisruptionBudget go.
#
# Emptying a node (a kernel update, a move): the PodDisruptionBudget (max_unavailable 0)
# makes `kubectl drain` wait on every host pod of the node. The coordinator's budget
# (min_available 1, coordinator.tf) lets a drain evict all but one of its replicas; the
# toolbox and web have NO budget: a drain evicts them at once. On ONE node the evicted
# replicas cannot reschedule and the last one blocks the drain, the toolbox goes, so the
# hosts would run with one coordinator replica and no worldctl and could not even be
# drained (that needs the coordinator and the toolbox): `just world-stop` FIRST, then
# drain the node (a host Deployment at 0 replicas has no pod to block it). With several
# nodes: drain and remove the node's hosts (`just world-drain <name>`, remove the key
# from var.hosts (adding new keys to take their place, on another node, if wanted),
# apply) before `kubectl drain`. The budget is an operations guard against an eviction
# that would turn a host into a crash; it is not what keeps two writers apart (the
# store's fencing is).

locals {
  # Per host: its pods' labels (the Deployment's selector); the Service and the budget
  # select a subset.
  host_pod_labels = {
    for k, _ in var.hosts : k => merge(local.host_labels, { "errantry/host" = k })
  }
}

resource "kubernetes_deployment_v1" "host" {
  for_each = var.hosts
  metadata {
    name      = "host-${each.key}"
    namespace = local.ns
    labels    = local.host_pod_labels[each.key]
  }
  spec {
    replicas = 1
    strategy {
      type = "Recreate"
    }
    selector {
      match_labels = local.host_pod_labels[each.key]
    }
    template {
      metadata {
        labels = local.host_pod_labels[each.key]
        annotations = {
          "checksum/secrets" = local.hosts_checksum
        }
      }
      spec {
        automount_service_account_token = false
        enable_service_links            = false
        # A leave cut is bounded by about 90 s (dev_r3.sh allows 95).
        termination_grace_period_seconds = 100
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
        volume {
          name = "service"
          secret {
            secret_name  = kubernetes_secret_v1.service.metadata[0].name
            default_mode = local.secret_mode
          }
        }
        # Only the coordinator's certificate: the host derives the pin from it.
        volume {
          name = "coordinator-cert"
          secret {
            secret_name  = kubernetes_secret_v1.link_identity["coordinator"].metadata[0].name
            default_mode = local.secret_mode
            items {
              key  = "${local.link_file}.cert"
              path = "${local.link_file}.cert"
            }
          }
        }
        # The peer identity: made at each pod's first start and published in the
        # `hosts` table at registration (a new pin per pod).
        volume {
          name = "peer"
          empty_dir {
            medium     = "Memory"
            size_limit = "1Mi"
          }
        }
        # cert-manager's Secret (charts/world-monitor): the pod starts once it exists.
        dynamic "volume" {
          for_each = var.client_certificate == null ? [] : ["client-tls"]
          content {
            name = "client-tls"
            secret {
              secret_name  = volume.value
              default_mode = local.secret_mode
            }
          }
        }
        container {
          name              = "host"
          image             = var.images.server
          image_pull_policy = "IfNotPresent"
          # The image's CMD `server` is PID 1, so SIGTERM reaches it: an exiting host
          # asks for a leave cut.
          # ORDER MATTERS: $(POD_IP) below expands only because POD_IP is declared first.
          env {
            name = "POD_IP"
            value_from {
              field_ref {
                field_path = "status.podIP"
              }
            }
          }
          env {
            name  = "VOXEL_ADVERTISE_PEER"
            value = "$(POD_IP):6500"
          }
          dynamic "env" {
            for_each = merge(local.server_env, {
              VOXEL_ROLE                = "host"
              VOXEL_HOST_NAME           = each.key
              VOXEL_ADVERTISE_QUIC      = "${var.public_host}:${each.value.quic_node_port}"
              VOXEL_ADVERTISE_WT        = "https://${var.public_host}:${each.value.wt_node_port}/"
              VOXEL_COORDINATOR_ADDR    = "${local.coordinator_fq}:6480"
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
            })
            content {
              name  = env.key
              value = env.value
            }
          }
          env {
            name = "VOXEL_WORLD_DB"
            value_from {
              secret_key_ref {
                name = kubernetes_secret_v1.db.metadata[0].name
                key  = "url"
              }
            }
          }
          port {
            name           = "quic"
            container_port = 6466
            protocol       = "UDP"
          }
          port {
            name           = "wt"
            container_port = 6467
            protocol       = "UDP"
          }
          port {
            name           = "peer"
            container_port = 6500
            protocol       = "UDP"
          }
          port {
            name           = "health"
            container_port = local.health_port
          }
          volume_mount {
            name       = "service"
            mount_path = local.service_token_dir
            read_only  = true
          }
          volume_mount {
            name       = "coordinator-cert"
            mount_path = local.coordinator_dir
            read_only  = true
          }
          volume_mount {
            name       = "peer"
            mount_path = local.peer_dir
          }
          dynamic "volume_mount" {
            for_each = var.client_certificate == null ? [] : ["client-tls"]
            content {
              name       = volume_mount.value
              mount_path = local.client_cert_dir
              read_only  = true
            }
          }
          security_context {
            allow_privilege_escalation = false
            capabilities {
              drop = ["ALL"]
            }
          }
          resources {
            requests = local.host_resources.requests
            limits   = local.host_resources.limits
          }
          # No startup probe: /livez is 200 until the process has registered, so a long
          # boot (a forge wait, a succession wait) never fails it. It fails only after
          # LIVEZ_STALL_MS (30 s) of a stalled registered loop, so this fires after about
          # a minute — long after the lease (10 s) has handed the host's land to an heir.
          # A host waiting for its forge or its succession, or drained, is live and unready.
          liveness_probe {
            http_get {
              path = "/livez"
              port = "health"
            }
            period_seconds    = 10
            failure_threshold = 3
          }
          readiness_probe {
            http_get {
              path = "/readyz"
              port = "health"
            }
            period_seconds = 5
          }
        }
      }
    }
  }
  # Unready until the world is forged (main.tf), and for good once drained: never wait
  # for the rollout. A Deployment past its progress deadline only reports it in its
  # status; it changes nothing and an apply does not read it.
  wait_for_rollout = false
  # Releases set the image (world_update.sh); an apply must not undo them. The replica
  # count is not ignored: an apply sets each host back to 1 (see the roots' headers).
  lifecycle {
    ignore_changes = [spec[0].template[0].spec[0].container[0].image]
  }
}

# What clients dial: one NodePort Service per host, selecting exactly its pod.
# externalTrafficPolicy Local keeps the client's source address (no SNAT).
resource "kubernetes_service_v1" "host_endpoint" {
  for_each = var.hosts
  metadata {
    name      = "host-${each.key}"
    namespace = local.ns
    # Not the hosts' component label: the ServiceMonitor must not select these.
    labels = merge(local.labels, {
      "app.kubernetes.io/component" = "host-endpoint"
      "errantry/host"               = each.key
    })
  }
  spec {
    type                    = "NodePort"
    external_traffic_policy = "Local"
    selector = {
      "app.kubernetes.io/component" = "host"
      "errantry/host"               = each.key
    }
    port {
      name        = "quic"
      port        = 6466
      target_port = "quic"
      node_port   = each.value.quic_node_port
      protocol    = "UDP"
    }
    port {
      name        = "wt"
      port        = 6467
      target_port = "wt"
      node_port   = each.value.wt_node_port
      protocol    = "UDP"
    }
  }
}

# A voluntary eviction (a node drain) of a host's pod waits: see the header.
resource "kubernetes_pod_disruption_budget_v1" "host" {
  for_each = var.hosts
  metadata {
    name      = "host-${each.key}"
    namespace = local.ns
    labels    = local.host_pod_labels[each.key]
  }
  spec {
    max_unavailable = "0"
    selector {
      match_labels = {
        "errantry/host" = each.key
      }
    }
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
