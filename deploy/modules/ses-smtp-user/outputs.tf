output "host" {
  value = "email-smtp.${data.aws_region.ses.region}.amazonaws.com"
}

output "port" {
  description = "STARTTLS."
  value       = 587
}

output "username" {
  value = aws_iam_access_key.smtp.id
}

output "password" {
  value     = aws_iam_access_key.smtp.ses_smtp_password_v4
  sensitive = true
}

output "sender" {
  value = var.sender
}
