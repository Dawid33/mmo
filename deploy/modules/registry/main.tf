# The orion cluster's private container images in ECR (this provider's region)
# and the one IAM user it pulls them with. The cluster is a Hetzner machine with
# no AWS instance role: it holds this user's access key and turns it into a pull
# token (valid 12 hours) itself (deploy/modules/login, CronJob `ecr-refresh`).
# Pushing is done by the owner's own (administrator) credentials, never by this user.

data "aws_region" "current" {}

resource "aws_ecr_repository" "repo" {
  for_each = toset(var.repositories)
  name     = each.value
  # Releases are tagged with the git short sha; `bootstrap` is moved by the
  # first push (scripts/orion_login_push.sh), hence mutable.
  image_tag_mutability = "MUTABLE"
  # Never delete a repository that still holds images.
  force_delete = false
  image_scanning_configuration {
    scan_on_push = true
  }
}

# Rule 1 matches every image tagged `bootstrap*` and keeps up to 9999 of them:
# an image matched by a higher-priority rule is never expired by a lower one, so
# the image the login module starts from survives rule 2. Rule 2 (tag status
# `any` must be the last rule) expires everything beyond the newest keep_images.
# The data source checks the document against ECR's lifecycle-policy schema.
data "aws_ecr_lifecycle_policy_document" "keep" {
  rule {
    priority    = 1
    description = "never expire the bootstrap image"
    selection {
      tag_status      = "tagged"
      tag_prefix_list = ["bootstrap"]
      count_type      = "imageCountMoreThan"
      count_number    = 9999
    }
    action {
      type = "expire"
    }
  }
  rule {
    priority    = 2
    description = "keep the newest ${var.keep_images} images"
    selection {
      tag_status   = "any"
      count_type   = "imageCountMoreThan"
      count_number = var.keep_images
    }
    action {
      type = "expire"
    }
  }
}

resource "aws_ecr_lifecycle_policy" "keep" {
  for_each   = aws_ecr_repository.repo
  repository = each.value.name
  policy     = data.aws_ecr_lifecycle_policy_document.keep.json
}

resource "aws_iam_user" "pull" {
  name = var.pull_user_name
  tags = { purpose = var.purpose }
}

# Pull only: GetAuthorizationToken cannot be scoped to a resource (it returns a
# token for the whole registry); what the token may fetch is limited by the
# second statement to these repositories.
resource "aws_iam_user_policy" "pull" {
  name = "ecr-pull"
  user = aws_iam_user.pull.name
  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [
      {
        Effect   = "Allow"
        Action   = ["ecr:GetAuthorizationToken"]
        Resource = "*"
      },
      {
        Effect = "Allow"
        Action = [
          "ecr:BatchCheckLayerAvailability",
          "ecr:GetDownloadUrlForLayer",
          "ecr:BatchGetImage",
        ]
        Resource = [for r in aws_ecr_repository.repo : r.arn]
      },
    ]
  })
}

# The secret lands in the state (encrypted S3). Rotate with
# `just orion-apply registry -replace=module.registry.aws_iam_access_key.pull`,
# then `just orion-apply login` (it copies the key into Secret ecr-pull-aws).
resource "aws_iam_access_key" "pull" {
  user = aws_iam_user.pull.name
}
