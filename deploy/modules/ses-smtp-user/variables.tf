variable "name" {
  description = "IAM user name."
  type        = string
}

variable "domain" {
  description = "An SES domain identity that ALREADY exists and is verified in the region of the `aws.ses` provider (factoriosurvivalworld.com: made, with DKIM / MAIL FROM / DMARC, by the survival-world infra in ~/Projects/mod)."
  type        = string
}

variable "sender" {
  description = "The one From address this login may send as, at that domain."
  type        = string
}

variable "purpose" {
  description = "Tag text. IAM allows letters, digits, spaces and _ . : / = + - @ only (no parentheses)."
  type        = string
}
