use login::mail::{
    LogMailer, Mailer, already_registered_mail, reset_mail, verify_mail, RESET_TOKEN_TTL_SECS,
    VERIFY_TOKEN_TTL_SECS,
};

#[test]
fn verify_mail_links_public_url() {
    let m = verify_mail("a@b.c", "https://x/login", "t");
    assert_eq!(m.to, "a@b.c");
    assert!(m.text.contains("https://x/login/verify?token=t"), "{}", m.text);
    let m = verify_mail("a@b.c", "https://x/login/", "t");
    assert!(m.text.contains("https://x/login/verify?token=t"));
    assert!(!m.text.contains("login//verify"));
}

#[test]
fn reset_mail_links() {
    let m = reset_mail("a@b.c", "https://x/login", "tok-_9");
    assert!(m.text.contains("https://x/login/reset?token=tok-_9"));
}

#[test]
fn already_registered_has_no_token() {
    let m = already_registered_mail("a@b.c", "https://x/login/");
    assert!(m.text.contains("https://x/login/reset"));
    assert!(!m.text.contains("token="));
}

#[test]
fn ttl_consts() {
    assert_eq!(VERIFY_TOKEN_TTL_SECS, 86_400);
    assert_eq!(RESET_TOKEN_TTL_SECS, 3_600);
}

#[tokio::test]
async fn log_mailer_records() {
    let l = LogMailer::default();
    l.send(verify_mail("a@b.c", "https://x", "t")).await.unwrap();
    let sent = l.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].to, "a@b.c");
}
