# Zulip (team chat) at https://<host>, from Zulip's own chart
# (github.com/zulip/docker-zulip, helm/zulip): one `zulip-0` pod plus the
# chart's bundled PostgreSQL, RabbitMQ, Redis and memcached. The PostgreSQL is
# deliberately the chart's own (Zulip's image, with its full-text search
# dictionaries) and NOT CloudNativePG. Orion has two CNPG clusters — pg for
# region/world databases and pg-management for login and Keycloak — while
# chat deliberately keeps Zulip's bundled PostgreSQL.
# Uploads, avatars and exports live in Garage, over S3 (s3.tf); the database and the data
# volume (settings) are local PVs. NOTHING is backed up yet (var.auto_backup).
#
# Outgoing mail goes through AWS SES (ses.tf) or any SMTP server (var.smtp).
# People log in with Zulip's own passwords, or through an OpenID Connect
# identity provider (oidc.tf).
#
# Settings reach Zulip as environment variables: SETTING_x -> settings.py,
# SECRETS_x -> zulip-secrets.conf, CONFIG_section__key -> zulip.conf
# (docs/reference/environment-vars.md in docker-zulip).

locals {
  ns = kubernetes_namespace_v1.zulip.metadata[0].name

  # One Secret for the four bundled services; the keys are the sub-charts' defaults.
  service_secret_keys = [
    "postgres-password",      # PostgreSQL superuser
    "password",               # PostgreSQL user `zulip`
    "rabbitmq-password",      #
    "rabbitmq-erlang-cookie", #
    "memcached-password",     #
    "redis-password",         #
  ]

  app_secrets = merge(
    {
      "secret-key"    = random_password.secret_key.result
      "s3-key"        = var.object_store_key.access_key
      "s3-secret-key" = var.object_store_key.secret_key
    },
    local.mail == null ? {} : { "email-password" = local.mail_password },
    var.oidc == null ? {} : { "social-auth-oidc-secret" = var.oidc_client_secret },
  )

  # Prometheus exporters of the bundled services (the Zulip server itself has
  # no metrics endpoint). Each sub-chart adds a sidecar exporter (RabbitMQ: its
  # own plugin) and a ServiceMonitor, which the monitoring root's Prometheus
  # picks up from any namespace.
  exporter_metrics = { for svc, image in var.exporter_images : svc => (
    var.metrics ? {
      enabled        = true
      image          = { repository = split(":", image)[0], tag = split(":", image)[1] }
      serviceMonitor = { enabled = true }
    } : null
  ) }
  rabbitmq_metrics = var.metrics ? {
    enabled        = true
    serviceMonitor = { default = { enabled = true } }
  } : null

  # zulip.environment takes a plain value or a `valueFrom` map.
  secret_env = { for env, key in {
    SECRETS_secret_key              = "secret-key"
    SECRETS_s3_key                  = "s3-key"
    SECRETS_s3_secret_key           = "s3-secret-key"
    SECRETS_email_password          = "email-password"
    SECRETS_social_auth_oidc_secret = "social-auth-oidc-secret"
    } : env => { valueFrom = { secretKeyRef = { name = "zulip-app", key = key } } }
    if(key != "email-password" || local.mail != null) && (key != "social-auth-oidc-secret" || var.oidc != null)
  }

  plain_env = merge(
    {
      SETTING_EXTERNAL_HOST       = var.host
      SETTING_ZULIP_ADMINISTRATOR = var.admin_email
      LOADBALANCER_IPS            = join(",", var.proxy_cidrs)
      AUTO_BACKUP_ENABLED         = var.auto_backup ? "True" : "False"

      # The S3 backend. "None" is written as Python's None: no local uploads.
      SETTING_LOCAL_UPLOADS_DIR      = "None"
      SETTING_S3_AUTH_UPLOADS_BUCKET = local.buckets.uploads
      SETTING_S3_AVATAR_BUCKET       = local.buckets.avatars
      SETTING_S3_EXPORT_BUCKET       = local.buckets.exports
      # Garage, not AWS: its address and region name, and path style
      # (https://<host>/<bucket>/<key>; Garage has no per-bucket hostnames here).
      SETTING_S3_ENDPOINT_URL     = var.object_store.url
      SETTING_S3_REGION           = var.object_store.region
      SETTING_S3_ADDRESSING_STYLE = "path"
      # Where browsers fetch avatars: the avatars bucket's website address.
      SETTING_S3_AVATAR_PUBLIC_URL_PREFIX = "${trimsuffix(var.object_store.avatars_url, "/")}/"

      CONFIG_application_server__queue_workers_multiprocess = var.queue_workers_multiprocess ? "True" : "False"
    },
    local.mail == null ? {} : {
      SETTING_EMAIL_HOST      = local.mail.host
      SETTING_EMAIL_HOST_USER = local.mail.user
      SETTING_EMAIL_PORT      = tostring(local.mail.port)
      SETTING_EMAIL_USE_TLS   = "True"
    },
    try(local.mail.noreply, null) == null ? {} : {
      SETTING_NOREPLY_EMAIL_ADDRESS         = local.mail.noreply
      SETTING_ADD_TOKENS_TO_NOREPLY_ADDRESS = "False"
    },
    local.oidc_env,
  )
}

resource "kubernetes_namespace_v1" "zulip" {
  metadata {
    name = var.namespace
  }
}

# Django's signing key. Replacing it logs everyone out.
resource "random_password" "secret_key" {
  length  = 50
  special = false
}

# Replacing one of these does NOT change the password inside an existing
# PostgreSQL / RabbitMQ volume: the service keeps the one it was created with.
resource "random_password" "service" {
  for_each = toset(local.service_secret_keys)
  length   = 32
  special  = false
}

resource "kubernetes_secret_v1" "app" {
  metadata {
    name      = "zulip-app"
    namespace = local.ns
  }
  data = local.app_secrets
}

resource "kubernetes_secret_v1" "services" {
  metadata {
    name      = "zulip-services"
    namespace = local.ns
  }
  data = { for k in local.service_secret_keys : k => random_password.service[k].result }
}

resource "helm_release" "zulip" {
  name       = "zulip"
  namespace  = local.ns
  repository = "oci://ghcr.io/zulip/helm-charts"
  chart      = "zulip"
  version    = var.chart_version
  # The first boot creates the database schema: several minutes.
  timeout = (var.startup_minutes + 5) * 60

  values = [yamlencode({
    zulip = {
      environment = merge(local.plain_env, local.secret_env)
      persistence = {
        storageClass = var.storage_class
        size         = var.volume_sizes["data"]
      }
    }

    # Env is read once at start; a changed Secret must restart the pod. Only
    # the hash is shown, so the plan still prints a readable values diff.
    podAnnotations = {
      "checksum/app-secret" = nonsensitive(sha256(jsonencode(local.app_secrets)))
    }

    resources = {
      requests = { cpu = "250m", memory = "2Gi" }
      limits   = { memory = var.memory_limits["zulip"] }
    }

    startupProbe = {
      periodSeconds    = 10
      failureThreshold = var.startup_minutes * 6
    }

    ingress = {
      enabled   = true
      className = var.ingress_class
      annotations = {
        "cert-manager.io/cluster-issuer" = var.cluster_issuer
      }
      hosts = [{ host = var.host, paths = [{ path = "/" }] }]
      tls   = [{ secretName = "zulip-tls", hosts = [var.host] }]
    }

    postgresql = {
      auth    = { existingSecret = kubernetes_secret_v1.services.metadata[0].name }
      # The sub-chart points the exporter at the role `postgres`, which the
      # zulip/zulip-postgresql image never creates (its superuser is
      # POSTGRES_USER = `zulip`): every scrape failed with `role "postgres" does
      # not exist`. A full DATA_SOURCE_NAME overrides the chart's user/password
      # /URI variables without naming one of them twice.
      metrics = local.exporter_metrics["postgres"] == null ? null : merge(local.exporter_metrics["postgres"], {
        extraEnvVars = [
          {
            name = "ZULIP_DB_PASSWORD"
            valueFrom = { secretKeyRef = {
              name = kubernetes_secret_v1.services.metadata[0].name
              key  = "password"
            } }
          },
          {
            name  = "DATA_SOURCE_NAME"
            value = "postgresql://zulip:$(ZULIP_DB_PASSWORD)@127.0.0.1:5432/zulip?sslmode=disable"
          },
        ]
      })
      primary = {
        persistence = {
          storageClass = var.storage_class
          size         = var.volume_sizes["postgres"]
        }
        resources = {
          requests = { cpu = "100m", memory = "256Mi" }
          limits   = { memory = var.memory_limits["postgres"] }
        }
      }
    }

    # Queues and caches keep nothing on disk (the chart's default): a restart
    # of one of them drops queued work, never messages.
    rabbitmq = {
      auth = {
        existingPasswordSecret = kubernetes_secret_v1.services.metadata[0].name
        existingErlangSecret   = kubernetes_secret_v1.services.metadata[0].name
      }
      metrics = local.rabbitmq_metrics
      resources = {
        requests = { cpu = "50m", memory = "256Mi" }
        limits   = { memory = var.memory_limits["rabbitmq"] }
      }
    }

    redis = {
      auth = {
        existingSecret            = kubernetes_secret_v1.services.metadata[0].name
        existingSecretPasswordKey = "redis-password"
      }
      metrics = local.exporter_metrics["redis"]
      master = {
        resources = {
          requests = { cpu = "20m", memory = "64Mi" }
          limits   = { memory = var.memory_limits["redis"] }
        }
      }
    }

    # No exporter here: memcached runs with SASL (the chart's auth), and
    # prometheus/memcached_exporter cannot authenticate (it has TLS flags only),
    # nor does the sub-chart let its sidecar take arguments. With it on, every
    # scrape timed out (`memcached_up` 0) and logged two errors.
    memcached = {
      auth = { existingPasswordSecret = kubernetes_secret_v1.services.metadata[0].name }
      resources = {
        requests = { cpu = "20m", memory = "64Mi" }
        limits   = { memory = var.memory_limits["memcached"] }
      }
    }
  })]

  depends_on = [
    kubernetes_secret_v1.app,
    aws_route53_record.zulip,
  ]

  lifecycle {
    precondition {
      condition     = var.smtp == null || var.smtp_password != null
      error_message = "smtp is set, so smtp_password is needed too."
    }
    precondition {
      condition     = var.smtp == null || var.ses == null
      error_message = "Set ses or smtp, not both."
    }
    precondition {
      condition     = var.oidc == null || var.oidc_client_secret != null
      error_message = "oidc is set, so oidc_client_secret is needed too."
    }
    precondition {
      condition     = var.password_login || var.oidc != null
      error_message = "password_login = false without oidc leaves no way to log in."
    }
  }
}
