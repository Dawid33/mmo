# The `players` realm: every player account. People sign up themselves with a
# username (their public name: 3-24 of A-Z a-z 0-9 _ -), an email and a password;
# the email must be verified before any login, because Zulip and the game match
# accounts on it. Passwords, verification and reset mail are Keycloak's; nothing
# else stores a password.

module "smtp" {
  source = "../ses-smtp-user"
  providers = {
    aws     = aws
    aws.ses = aws.ses
  }
  name    = var.smtp_iam_user_name
  domain  = var.mail_domain
  sender  = var.mail_sender
  purpose = "orion Keycloak account mail via SES SMTP - deploy/modules/keycloak-realm"
}

resource "keycloak_realm" "players" {
  realm        = var.realm
  enabled      = true
  display_name = var.display_name

  registration_allowed           = true
  registration_email_as_username = false
  login_with_email_allowed       = true
  duplicate_emails_allowed       = false
  verify_email                   = true
  reset_password_allowed         = true
  remember_me                    = true
  edit_username_allowed          = false
  ssl_required                   = "external"
  login_theme                    = var.login_theme

  security_defenses {
    brute_force_detection {
      max_login_failures = 10
    }
  }

  smtp_server {
    host              = module.smtp.host
    port              = module.smtp.port
    starttls          = true
    from              = module.smtp.sender
    from_display_name = var.display_name
    auth {
      username = module.smtp.username
      password = module.smtp.password
    }
  }
}

# What a sign-up asks for. No first / last name: the username is the name.
resource "keycloak_realm_user_profile" "players" {
  realm_id = keycloak_realm.players.id

  attribute {
    name               = "username"
    display_name       = "$${username}"
    required_for_roles = ["user"]
    permissions {
      view = ["admin", "user"]
      edit = ["admin", "user"]
    }
    validator {
      name   = "length"
      config = { min = "3", max = "24" }
    }
    validator {
      name   = "pattern"
      config = { pattern = "^[A-Za-z0-9_-]+$", "error-message" = "Use letters, digits, _ and - only." }
    }
    validator {
      name = "username-prohibited-characters"
    }
  }

  attribute {
    name               = "email"
    display_name       = "$${email}"
    required_for_roles = ["user"]
    permissions {
      view = ["admin", "user"]
      edit = ["admin", "user"]
    }
    validator {
      name = "email"
    }
    validator {
      name   = "length"
      config = { max = "254" }
    }
  }
}
