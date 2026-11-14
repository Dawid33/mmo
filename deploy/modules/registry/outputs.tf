output "repository_urls" {
  description = "Repository name -> URL (<account>.dkr.ecr.<region>.amazonaws.com/<name>)."
  value       = { for k, r in aws_ecr_repository.repo : k => r.repository_url }
}

output "registry_host" {
  description = "The registry's host name (what `docker login` and a dockerconfigjson are keyed by)."
  value       = "${values(aws_ecr_repository.repo)[0].registry_id}.dkr.ecr.${data.aws_region.current.region}.amazonaws.com"
}

output "region" {
  value = data.aws_region.current.region
}

output "pull_access_key_id" {
  value = aws_iam_access_key.pull.id
}

output "pull_secret_access_key" {
  value     = aws_iam_access_key.pull.secret
  sensitive = true
}
