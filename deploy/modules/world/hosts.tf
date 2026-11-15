# The region hosts (VOXEL_ROLE=host): StatefulSet hosts, pods hosts-0 … hosts-<n-1>,
# one per entry of var.hosts. A pod's ordinal and the addresses clients are told
# come from its own file in ConfigMap host-endpoints, read by the shell before it
# execs the server (PID 1, so SIGTERM reaches it: an exiting host asks for a
# leave cut).
#
# Differences from scripts/dev_r3.sh, on purpose: dev restarts a host only on exit
# 5 or 137 (and 3 under claim) and gives up after 3 restarts in 60 s; here every
# exit restarts (restartPolicy Always: 0, 1, 3, 4, 5, 137) and Kubernetes never
# gives up (CrashLoopBackOff, up to 5 min apart) — an alert watches restart loops
# instead. Dev starts hosts after the coordinator's pin file appears; here the pin
# comes from the coordinator's certificate in a Secret and a host dials until the
# coordinator answers.
#
# Parallel pod management: a boot waiting on a succession (unready, maybe for
# minutes) must not hold back the others. OnDelete updates: a template change
# reaches a host only when its pod is deleted (world_update.sh scales to 0).
# No CPU limit: a throttled main loop is a missed lease heartbeat.

resource "kubernetes_config_map_v1" "host_endpoints" {
  metadata {
    name      = "host-endpoints"
    namespace = local.ns
    labels    = local.labels
  }
  data = {
    for i, h in var.hosts : "hosts-${i}.env" => <<-EOT
      VOXEL_HOST_ORDINAL=${i}
      VOXEL_ADVERTISE_QUIC=${var.public_host}:${h.quic_node_port}
      VOXEL_ADVERTISE_WT=https://${var.public_host}:${h.wt_node_port}/
    EOT
  }
}

resource "kubernetes_stateful_set_v1" "hosts" {
  metadata {
    name      = "hosts"
    namespace = local.ns
    labels    = local.host_labels
  }
  spec {
    service_name          = kubernetes_service_v1.hosts.metadata[0].name
    replicas              = length(var.hosts)
    pod_management_policy = "Parallel"
    update_strategy {
      type = "OnDelete"
    }
    selector {
      match_labels = local.host_labels
    }
    template {
      metadata {
        labels = local.host_labels
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
        volume {
          name = "host-link-identity"
          secret {
            secret_name  = kubernetes_secret_v1.link_identity["host-link"].metadata[0].name
            default_mode = local.secret_mode
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
        volume {
          name = "host-endpoints"
          config_map {
            name = kubernetes_config_map_v1.host_endpoints.metadata[0].name
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
          # $HOSTNAME is the pod name (hosts-<i>); the shell expands it.
          command = ["sh", "-c", "set -a; . ${local.host_env_dir}/$HOSTNAME.env; set +a; exec server"]
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
              VOXEL_ROLE                     = "host"
              VOXEL_COORDINATOR_ADDR         = "${local.coordinator_fq}:6480"
              VOXEL_BIND_ADDR                = "0.0.0.0"
              VOXEL_SERVER_PORT              = "6466"
              VOXEL_WT_PORT                  = "6467"
              VOXEL_PEER_BIND                = "0.0.0.0:6500"
              VOXEL_PEER_CERT_HASH_FILE      = "${local.peer_dir}/peer.cert-sha256"
              VOXEL_ENTRY_HOST               = "0"
              VOXEL_HOST_LINK_BIND           = "0.0.0.0:6490"
              VOXEL_HOST_LINK_CERT_HASH_FILE = "${local.host_link_dir}/${local.link_file}"
              VOXEL_AUTH                     = "jwt"
              VOXEL_WORLD_ID                 = var.world_id
              VOXEL_LOGIN_URL                = var.auth.login_url
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
            name           = "link"
            container_port = 6490
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
            name       = "host-link-identity"
            mount_path = local.host_link_dir
            read_only  = true
          }
          volume_mount {
            name       = "peer"
            mount_path = local.peer_dir
          }
          volume_mount {
            name       = "host-endpoints"
            mount_path = local.host_env_dir
            read_only  = true
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
          # No startup probe. /livez fails only after LIVEZ_STALL_MS (30 s) of a
          # stalled registered loop, so this fires after about a minute — long after
          # the lease (10 s) has handed the host's land to an heir. A host waiting
          # for its forge or its succession is live and unready.
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
  # Unready until the world is forged (main.tf): never wait for the rollout.
  wait_for_rollout = false
  # Releases set the image (world_update.sh); an apply must not undo them.
  lifecycle {
    ignore_changes = [spec[0].template[0].spec[0].container[0].image]
  }
}

# Headless: peers dial each other's pod IPs (VOXEL_ADVERTISE_PEER), and the login
# service dials hosts-0.hosts.<ns>.svc.cluster.local:6490 (output host_link), also
# while the pod is unready. The health port is for the ServiceMonitor.
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
      name        = "peer"
      port        = 6500
      target_port = "peer"
      protocol    = "UDP"
    }
    port {
      name        = "link"
      port        = 6490
      target_port = "link"
      protocol    = "UDP"
    }
    port {
      name        = "health"
      port        = local.health_port
      target_port = "health"
    }
  }
}

# What clients dial: one NodePort Service per host, selecting exactly its pod.
# externalTrafficPolicy Local keeps the client's source address (no SNAT).
resource "kubernetes_service_v1" "host" {
  for_each = { for i, h in var.hosts : tostring(i) => h }
  metadata {
    name      = "host-${each.key}"
    namespace = local.ns
    # Not the hosts' component label: the ServiceMonitor must not select these.
    labels = merge(local.labels, { "app.kubernetes.io/component" = "host-endpoint" })
  }
  spec {
    type                    = "NodePort"
    external_traffic_policy = "Local"
    selector = {
      "statefulset.kubernetes.io/pod-name" = "hosts-${each.key}"
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
