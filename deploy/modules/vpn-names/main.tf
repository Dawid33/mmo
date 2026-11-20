# Names that answer only over the operator VPN (docs/superpowers/plans/2026-11-15-orion-vpn.md):
# <name>.<vpn_domain> -> a Service's ClusterIP, with a real Let's Encrypt certificate.
#
# - CoreDNS (k3s's `coredns-custom` import hook) rewrites each <name>.<vpn_domain> to the
#   Service's in-cluster name, so it resolves to the ClusterIP — reachable only from the
#   pod network and the VPN (the VPN peers send <vpn_domain> lookups to CoreDNS: split DNS).
#   Public DNS has no records for these names at all.
# - A DNS-01 ClusterIssuer (`issuer_name`) proves the names to Let's Encrypt with TXT
#   records in Route 53 (HTTP-01 cannot: nothing public answers for them). Its IAM user
#   may only touch TXT records named _acme-challenge.*<vpn_domain> in that one zone.
#
# A Service using a name runs its own TLS on the port the URL uses (Keycloak: 443 -> 8443)
# with a Certificate from this issuer in its own namespace.

data "aws_route53_zone" "zone" {
  name = var.dns_zone
}

resource "aws_iam_user" "dns01" {
  name = var.iam_user_name
  tags = { purpose = "cert-manager DNS-01 for ${var.vpn_domain} on orion" } # no parentheses: IAM tag values reject them
}

resource "aws_iam_user_policy" "dns01" {
  name = "acme-dns01"
  user = aws_iam_user.dns01.name
  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [
      {
        Effect   = "Allow"
        Action   = ["route53:GetChange"]
        Resource = "arn:aws:route53:::change/*"
      },
      {
        Effect   = "Allow"
        Action   = ["route53:ListResourceRecordSets"]
        Resource = data.aws_route53_zone.zone.arn
      },
      {
        Effect   = "Allow"
        Action   = ["route53:ChangeResourceRecordSets"]
        Resource = data.aws_route53_zone.zone.arn
        Condition = {
          "ForAllValues:StringEquals" = { "route53:ChangeResourceRecordSetsRecordTypes" = ["TXT"] }
          "ForAllValues:StringLike"   = { "route53:ChangeResourceRecordSetsNormalizedRecordNames" = ["_acme-challenge.*${var.vpn_domain}"] }
        }
      },
    ]
  })
}

# Rotate with `tofu apply -replace=module.vpn_names.aws_iam_access_key.dns01`.
resource "aws_iam_access_key" "dns01" {
  user = aws_iam_user.dns01.name
}

resource "kubernetes_secret_v1" "dns01" {
  metadata {
    name      = "${var.issuer_name}-route53"
    namespace = var.cert_manager_namespace
  }
  data = {
    "secret-access-key" = aws_iam_access_key.dns01.secret
  }
}

# The ClusterIssuer is a custom resource: a tiny local chart, as in the cert-manager module.
resource "helm_release" "issuer" {
  name      = var.issuer_name
  namespace = var.cert_manager_namespace
  chart     = "${path.module}/charts/dns01-issuer"

  values = [yamlencode({
    name         = var.issuer_name
    server       = var.acme_server
    region       = var.aws_region
    hostedZoneID = data.aws_route53_zone.zone.zone_id
    accessKeyID  = aws_iam_access_key.dns01.id
    secretName   = kubernetes_secret_v1.dns01.metadata[0].name
    secretKey    = "secret-access-key"
    dnsZones     = [var.vpn_domain]
  })]
}

# k3s's CoreDNS imports /etc/coredns/custom/*.override into its main server block
# (the ConfigMap is optional and not managed by k3s). `answer auto` gives the reply the
# asked-for name back, so resolvers accept it.
resource "kubernetes_config_map_v1" "coredns_custom" {
  metadata {
    name      = "coredns-custom"
    namespace = "kube-system"
  }
  data = {
    "vpn-names.override" = join("\n", concat(
      ["# Managed by OpenTofu: deploy/modules/vpn-names (orion root vpn-names)."],
      [for name, target in var.names :
      "rewrite name exact ${name}.${var.vpn_domain} ${target} answer auto"],
    ))
  }
}
