//! Mail for the login service: the [`Mailer`] trait, a dev/test [`LogMailer`], the Amazon SES v2
//! [`SesMailer`], and the verify / reset / already-registered message builders.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

/// How long a verification token lives.
pub const VERIFY_TOKEN_TTL_SECS: i64 = 86_400;
/// How long a password-reset token lives.
pub const RESET_TOKEN_TTL_SECS: i64 = 3_600;

pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

/// One plain-text message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mail {
    pub to: String,
    pub subject: String,
    pub text: String,
}

/// A failed send. Carries the transport's message, never the mail body (it holds a token link).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("mail not sent: {0}")]
pub struct MailError(pub String);

pub trait Mailer: Send + Sync {
    fn send(&self, mail: Mail) -> BoxFuture<Result<(), MailError>>;
}

/// Logs each mail's recipient, subject and link line at info and keeps the mail in memory.
///
/// DEV / TESTS ONLY: the link carries a single-use email token, so this must never be used in
/// production.
#[derive(Clone, Default)]
pub struct LogMailer {
    sent: Arc<Mutex<Vec<Mail>>>,
}

impl LogMailer {
    pub fn sent(&self) -> Vec<Mail> {
        self.sent.lock().unwrap().clone()
    }
}

impl Mailer for LogMailer {
    fn send(&self, mail: Mail) -> BoxFuture<Result<(), MailError>> {
        let link = mail
            .text
            .lines()
            .find(|l| l.trim_start().starts_with("http"))
            .unwrap_or("")
            .trim();
        log::info!("[mail] to={} subject={:?} link={}", mail.to, mail.subject, link);
        self.sent.lock().unwrap().push(mail);
        Box::pin(async { Ok(()) })
    }
}

/// Sends through Amazon SES v2 (`SendEmail`, simple text body). Credentials and region come from
/// the AWS default chain.
pub struct SesMailer {
    client: aws_sdk_sesv2::Client,
    from: String,
}

impl SesMailer {
    pub async fn from_env(from: String) -> SesMailer {
        use aws_smithy_http_client::{Builder, tls};
        // Explicit rustls+ring client: the SDK's default one would pull in aws-lc-rs.
        let http = Builder::new()
            .tls_provider(tls::Provider::Rustls(tls::rustls_provider::CryptoMode::Ring))
            .build_https();
        let conf = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .http_client(http)
            .load()
            .await;
        SesMailer { client: aws_sdk_sesv2::Client::new(&conf), from }
    }
}

impl Mailer for SesMailer {
    fn send(&self, mail: Mail) -> BoxFuture<Result<(), MailError>> {
        use aws_sdk_sesv2::types::{Body, Content, Destination, EmailContent, Message};
        let client = self.client.clone();
        let from = self.from.clone();
        Box::pin(async move {
            let content = |s: String| {
                Content::builder().data(s).charset("UTF-8").build().map_err(|e| MailError(e.to_string()))
            };
            let message = Message::builder()
                .subject(content(mail.subject)?)
                .body(Body::builder().text(content(mail.text)?).build())
                .build();
            client
                .send_email()
                .from_email_address(from)
                .destination(Destination::builder().to_addresses(mail.to).build())
                .content(EmailContent::builder().simple(message).build())
                .send()
                .await
                .map_err(|e| MailError(aws_sdk_sesv2::error::DisplayErrorContext(&e).to_string()))?;
            Ok(())
        })
    }
}

fn base(public_url: &str) -> &str {
    public_url.trim_end_matches('/')
}

/// Tokens are base64url (URL-safe); no encoding is applied.
pub fn verify_mail(to: &str, public_url: &str, token: &str) -> Mail {
    Mail {
        to: to.to_string(),
        subject: "Verify your email address".to_string(),
        text: format!(
            "Confirm your email address by opening this link (valid for {} hours):\n\n{}/verify?token={}\n\nIf you did not create an account, ignore this mail.\n",
            VERIFY_TOKEN_TTL_SECS / 3600,
            base(public_url),
            token
        ),
    }
}

pub fn reset_mail(to: &str, public_url: &str, token: &str) -> Mail {
    Mail {
        to: to.to_string(),
        subject: "Reset your password".to_string(),
        text: format!(
            "Choose a new password by opening this link (valid for {} hour):\n\n{}/reset?token={}\n\nIf you did not ask for this, ignore this mail.\n",
            RESET_TOKEN_TTL_SECS / 3600,
            base(public_url),
            token
        ),
    }
}

/// Sent instead of a verify mail when someone registers an address that already has an account
/// (the HTTP answer stays the same, so registration does not reveal which addresses exist).
/// Carries no token.
pub fn already_registered_mail(to: &str, public_url: &str) -> Mail {
    Mail {
        to: to.to_string(),
        subject: "You already have an account".to_string(),
        text: format!(
            "Someone tried to register this address, but it already has an account.\nIf that was you, sign in, or request a password reset at:\n\n{}/reset\n\nIf it was not you, ignore this mail.\n",
            base(public_url)
        ),
    }
}
