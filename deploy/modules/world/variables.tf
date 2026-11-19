variable "world_id" {
  description = "The world's id: also its id in the login service's registry (VOXEL_WORLD_ID) and, by default, its namespace's suffix."
  type        = string
  validation {
    condition     = can(regex("^[a-z][a-z0-9]{0,30}$", var.world_id))
    error_message = "world_id: a lower-case letter, then up to 30 lower-case letters or digits (as the postgres module's world ids)."
  }
}

variable "namespace" {
  description = "Namespace the world lives in; the module creates it. null = \"world-<world_id>\"."
  type        = string
  default     = null
}

variable "images" {
  description = "Full image references the workloads are CREATED with: server (mmo/server: coordinator, fleet, toolbox; a host is created on the coordinator's image of that moment) and web (mmo/web). Releases set digests with `kubectl set image` (scripts/world_update.sh) and every workload ignores later changes to its image, so changing these after the first apply does nothing."
  type        = object({ server = string, web = string })
}

variable "image_pull_secret" {
  description = "Name of a dockerconfigjson Secret in the namespace every pod lists in imagePullSecrets (the ecr-pull module's secret_name output, which also orders it before the pods). null = none (images already on the node)."
  type        = string
  default     = null
}

variable "database_url" {
  description = "The world's PostgreSQL URL (the postgres module's world_urls[world_id]). Reaches pods only through Secret world-db."
  type        = string
  sensitive   = true
}

variable "service_token" {
  description = "The login service's service token (login module output service_token): coordinator and hosts present it on their links. Reaches pods only as a mounted file."
  type        = string
  sensitive   = true
}

variable "fleet" {
  description = <<-EOT
    The world's region hosts (Elastic 4): the coordinator keeps between min and max of them in
    the table desired_hosts (VOXEL_FLEET_MIN / _MAX, and the pace dials VOXEL_SCALE_UP_AFTER_MS,
    VOXEL_SCALE_DOWN_AFTER_MS, VOXEL_HOST_MIN_LIFETIME_MS when set; null = the server's
    defaults), and the reconciler (fleet.tf) runs one Deployment host-<name> per row. A row's
    slot s gives its NodePort Service the UDP node ports port_base + 2s (QUIC) and
    port_base + 2s + 1 (WebTransport): the range port_base .. port_base + 2·max - 1 (output
    client_ports) must be open to clients. min 0 lets the last host go when the world is idle,
    and needs max >= 2 (a last host that left holding in-flight rows keeps its slot until the
    next host inherits them).
    A coordinator-only change: an apply restarts the coordinator's replicas, no host.
  EOT
  type = object({
    min                  = optional(number, 1)
    max                  = number
    port_base            = number
    scale_up_after_ms    = optional(number)
    scale_down_after_ms  = optional(number)
    host_min_lifetime_ms = optional(number)
  })
  validation {
    condition = alltrue([
      for v in [var.fleet.min, var.fleet.max, var.fleet.port_base] : floor(v) == v
    ])
    error_message = "fleet: min, max and port_base are whole numbers."
  }
  validation {
    condition     = var.fleet.min >= 0 && var.fleet.min <= var.fleet.max && var.fleet.max >= 1
    error_message = "fleet: 0 <= min <= max, and max >= 1 (the coordinator refuses to start on min > max)."
  }
  validation {
    condition     = var.fleet.min != 0 || var.fleet.max >= 2
    error_message = "fleet: min 0 needs max >= 2: a last host that left still holding in-flight rows keeps its slot until the next host inherits them, so with one slot the world could never start again (the coordinator refuses to start too)."
  }
  validation {
    condition     = var.fleet.port_base >= 30000 && var.fleet.port_base + 2 * var.fleet.max - 1 <= 32767
    error_message = "fleet: the node ports port_base .. port_base + 2*max - 1 must lie within 30000-32767."
  }
  validation {
    condition = alltrue([
      for v in [var.fleet.scale_up_after_ms, var.fleet.scale_down_after_ms, var.fleet.host_min_lifetime_ms] :
      v == null || try(v > 0 && floor(v) == v, false)
    ])
    error_message = "fleet: scale_up_after_ms, scale_down_after_ms and host_min_lifetime_ms are whole numbers of milliseconds above 0 (the server refuses 0), or null."
  }
}

variable "public_host" {
  description = "The name or address clients dial (VOXEL_ADVERTISE_QUIC / _WT), e.g. game.errantry.renownandruin.com."
  type        = string
}

variable "client_certificate" {
  description = "The client-facing certificate of the hosts. null = each host's self-signed one, pinned by clients; set = a cert-manager Certificate client-tls for public_host from this ClusterIssuer (needs cert-manager and the DNS record first). The web Ingress gets TLS from the same issuer only when this is set."
  type        = object({ issuer = string })
  default     = null
}

variable "web" {
  description = "The browser bundle (mmo/web): the Ingress host it is served on, and the login service's in-cluster URL its /login proxy forwards to (LOGIN_UPSTREAM). null = no web."
  type        = object({ ingress_host = string, login_upstream = string })
  default     = null
}

variable "auth" {
  description = "The login service hosts verify join tickets with (VOXEL_LOGIN_URL). VOXEL_AUTH is always jwt: several hosts need the login link."
  type        = object({ login_url = string })
}

variable "ownership" {
  description = "Ownership mode of the coordinator and every host (one value: they must agree; main.tf's ownership_env is its one definition)."
  type        = string
  default     = "claim"
  validation {
    condition     = contains(["static", "claim"], var.ownership)
    error_message = "ownership: static or claim (allinone is the single-process mode)."
  }
  validation {
    condition     = var.ownership != "static" || (var.fleet.min == 1 && var.fleet.max == 1)
    error_message = "ownership: static runs ONE host (a second registers beside a live row and is refused: NotAlone, a crash loop): set fleet.min = fleet.max = 1, or use claim."
  }
}

variable "coordinator_replicas" {
  description = "Coordinator replicas (Elastic 2b): each one a process of its own with a replica id the store issues, all sharing the same link identity (Secret coordinator-identity: one pin for every replica) behind the ClusterIP Service. Two keep saves, logins and successions going through a dead coordinator pod; their writes are fenced by the coordinator row's version, never by this count. A PodDisruptionBudget (min_available 1) exists only above 1. world-stop records the count in the Deployment's annotation errantry/replicas and world-start restores it."
  type        = number
  default     = 2
  validation {
    condition     = var.coordinator_replicas >= 1 && floor(var.coordinator_replicas) == var.coordinator_replicas
    error_message = "coordinator_replicas: a whole number, at least 1 (a stopped world is world-stop's business, not 0 here)."
  }
}

variable "save_secs" {
  description = "VOXEL_SAVE_SECS: the world-cut period."
  type        = number
  default     = 60
}

variable "super_region_regions" {
  description = "VOXEL_SUPER_REGION_REGIONS: checked against the world row (worldctl super-region before the first owned row)."
  type        = number
  default     = 16
}

variable "land_idle_ms" {
  description = "VOXEL_LAND_IDLE_MS on every host: how long a host's land stays idle (no player, nothing in flight) before the host releases it. null = unset, the server's default (300000, 5 min). A host-only change: it goes through cuts on a running world."
  type        = number
  default     = null
  validation {
    condition     = var.land_idle_ms == null || try(var.land_idle_ms > 0 && floor(var.land_idle_ms) == var.land_idle_ms, false)
    error_message = "land_idle_ms: a whole number of milliseconds above 0 (the server refuses 0), or null."
  }
}

variable "metrics" {
  description = "A ServiceMonitor for the coordinator's, the hosts' and the fleet reconciler's /metrics. Needs the Prometheus operator's CRDs."
  type        = bool
  default     = true
}

variable "resources" {
  description = "Per role: { cpu = \"<request>\", memory = \"<request = limit>\" }. No CPU limit (a throttled main loop is a missed heartbeat). toolbox = { memory = \"<limit>\" } (default 1Gi): the forge runs there (`worldctl forge`; an OOM is exit 137 with nothing written); its requests stay small."
  type = object({
    coordinator = any
    host        = any
    toolbox     = optional(object({ memory = string }), { memory = "1Gi" })
  })
  validation {
    condition = alltrue([
      for r in [var.resources.coordinator, var.resources.host] : can(tostring(r.cpu)) && can(tostring(r.memory))
    ])
    error_message = "resources: coordinator and host each need cpu and memory."
  }
}
