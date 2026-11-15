# A send-only SMTP login for AWS SES: an IAM user that may send, through one
# existing domain identity, as one From address. The identity and its DNS are
# not made here. (deploy/modules/zulip/ses.tf predates this module and holds
# its own copy of the same thing.)

data "aws_region" "ses" {
  provider = aws.ses
}

data "aws_sesv2_email_identity" "site" {
  provider       = aws.ses
  email_identity = var.domain
}

resource "aws_iam_user" "smtp" {
  name = var.name
  tags = { purpose = var.purpose }
}

resource "aws_iam_user_policy" "smtp" {
  name = "send-email"
  user = aws_iam_user.smtp.name
  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Effect   = "Allow"
      Action   = ["ses:SendRawEmail", "ses:SendEmail"]
      Resource = data.aws_sesv2_email_identity.site.arn
      Condition = {
        StringEquals = { "ses:FromAddress" = var.sender }
      }
    }]
  })
}

# The SMTP password is derived from the secret key FOR THE REGION of this
# provider, hence aws.ses. Rotate with `tofu apply -replace=` on this resource.
resource "aws_iam_access_key" "smtp" {
  provider = aws.ses
  user     = aws_iam_user.smtp.name
}
