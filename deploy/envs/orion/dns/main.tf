# The renownandruin.com records no module makes: the game, the website, PicoShare, the base
# name and the two apex names that redirect to the website. Every other host's record is made
# by its own module from its `dns_zone` variable. Plan: docs/superpowers/plans/2026-11-14-hosts-to-errantry-renownandruin.md.
# Apply this root first; it needs the zone's delegation to have finished.

variable "zone_name" {
  description = "The Route 53 zone (registered through Route 53)."
  type        = string
  default     = "renownandruin.com"
}

variable "base" {
  description = "The base name every service host sits under."
  type        = string
  default     = "errantry.renownandruin.com"
}

variable "node_ip" {
  description = "The orion box's public address."
  type        = string
  default     = "5.9.7.138"
}

# The zone exists; it is looked up, not managed, like every module does.
data "aws_route53_zone" "game" {
  name = var.zone_name
}

resource "aws_route53_record" "host" {
  for_each = toset([
    "game.${var.base}",
    var.base,
    "www.${var.base}",
    "share.${var.base}",
    var.zone_name,
    "www.${var.zone_name}",
  ])
  zone_id = data.aws_route53_zone.game.zone_id
  name    = each.key
  type    = "A"
  ttl     = 300
  records = [var.node_ip]
}

output "hosts" {
  description = "The record names this root manages."
  value       = sort([for r in aws_route53_record.host : r.name])
}
