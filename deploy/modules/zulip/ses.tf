# Outgoing mail through AWS SES (var.ses). The domain identity, its DKIM,
# MAIL FROM and DMARC records are NOT made here: they belong to the
# survival-world infra (~/Projects/mod/mods/survival-world/infra), which owns
# the domain. This file only adds Zulip's own SMTP login: an IAM user that may
# send, through that one identity, as that one From address — separate from the
# website's `survival-world-smtp` user, which may only send as its own address.

data "aws_region" "ses" {
  provider = aws.ses
}

data "aws_sesv2_email_identity" "site" {
  count          = var.ses == null ? 0 : 1
  provider       = aws.ses
  email_identity = var.ses.domain
}

resource "aws_iam_user" "smtp" {
  count = var.ses == null ? 0 : 1
  name  = "${var.iam_user_name}-smtp"
  tags  = { purpose = "orion k3s Zulip outgoing mail via SES SMTP - deploy/modules/zulip" }
}

resource "aws_iam_user_policy" "smtp" {
  count = var.ses == null ? 0 : 1
  name  = "send-email"
  user  = aws_iam_user.smtp[0].name
  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Effect   = "Allow"
      Action   = ["ses:SendRawEmail", "ses:SendEmail"]
      Resource = data.aws_sesv2_email_identity.site[0].arn
      Condition = {
        StringEquals = { "ses:FromAddress" = var.ses.sender }
      }
    }]
  })
}

# The SMTP password is derived from the secret key FOR THE REGION of this
# provider, hence aws.ses. Rotate: tofu apply -replace=module.zulip.aws_iam_access_key.smtp[0]
resource "aws_iam_access_key" "smtp" {
  count    = var.ses == null ? 0 : 1
  provider = aws.ses
  user     = aws_iam_user.smtp[0].name
}

locals {
  # One shape for both ways of sending; null = no mail.
  mail = var.ses != null ? {
    host    = "email-smtp.${data.aws_region.ses.region}.amazonaws.com"
    user    = aws_iam_access_key.smtp[0].id
    port    = 587
    noreply = var.ses.sender
  } : var.smtp
  mail_password = var.ses != null ? aws_iam_access_key.smtp[0].ses_smtp_password_v4 : var.smtp_password
}
