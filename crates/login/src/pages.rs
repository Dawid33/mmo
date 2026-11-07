//! The plain-HTML pages mailed links open (no JavaScript): `GET /verify?token=` confirms an
//! address; `GET /reset?token=` shows a new-password form that POSTs to `/reset`; `GET /reset`
//! without a token shows a form asking for the address, which POSTs to `/reset/request`.
//! Everything echoed back is HTML-escaped.

use axum::extract::{Form, Query, State};
use axum::http::StatusCode;
use axum::response::Html;
use axum::routing::{get, post};
use axum::Router;
use login_api::{PASSWORD_MAX_CHARS, PASSWORD_MIN_CHARS};
use serde::Deserialize;

use crate::api::{AppState, Fail, confirm_email, confirm_reset, request_reset};
use crate::password::check_password;

/// The page that confirms an address.
pub const PAGE_VERIFY: &str = "/verify";
/// The new-password page and its form's target.
pub const PAGE_RESET: &str = "/reset";
/// The "mail me a reset link" form's target.
pub const PAGE_RESET_REQUEST: &str = "/reset/request";

#[derive(Deserialize)]
struct TokenQuery {
    token: Option<String>,
}

#[derive(Deserialize)]
struct ResetForm {
    #[serde(default)]
    token: String,
    #[serde(default)]
    password: String,
}

#[derive(Deserialize)]
struct ResetRequestForm {
    #[serde(default)]
    email: String,
}

type Page = (StatusCode, Html<String>);

/// Minimal HTML escaping for text and attribute values.
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// `body` is trusted HTML (the callers escape what they put in).
fn page(status: StatusCode, title: &str, body: &str) -> Page {
    let title = escape(title);
    (
        status,
        Html(format!(
            "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">\
             <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
             <title>{title}</title></head>\n<body style=\"font-family: sans-serif; max-width: 32rem; margin: 2rem auto; padding: 0 1rem\">\
             <h1>{title}</h1>\n{body}\n</body></html>\n"
        )),
    )
}

fn failed(title: &str, f: Fail) -> Page {
    page(f.status, title, &format!("<p>{}</p>", escape(&f.message)))
}

fn reset_form(token: &str, note: Option<&str>) -> String {
    let note = note.map(|n| format!("<p>{}</p>\n", escape(n))).unwrap_or_default();
    format!(
        "{note}<form method=\"post\" action=\"{PAGE_RESET}\">\n\
         <input type=\"hidden\" name=\"token\" value=\"{}\">\n\
         <p><label>New password ({PASSWORD_MIN_CHARS}–{PASSWORD_MAX_CHARS} characters)<br>\
         <input type=\"password\" name=\"password\" minlength=\"{PASSWORD_MIN_CHARS}\" maxlength=\"{PASSWORD_MAX_CHARS}\" \
         autocomplete=\"new-password\" required></label></p>\n\
         <p><button type=\"submit\">Change password</button></p>\n</form>",
        escape(token)
    )
}

async fn verify_page(State(s): State<AppState>, Query(q): Query<TokenQuery>) -> Page {
    let Some(token) = q.token.filter(|t| !t.is_empty()) else {
        return page(StatusCode::BAD_REQUEST, "Email not verified", "<p>The link has no token.</p>");
    };
    match confirm_email(&s, &token).await {
        Ok(()) => page(
            StatusCode::OK,
            "Email verified",
            "<p>Your email address is verified. You can close this page and log in from the game.</p>",
        ),
        Err(f) => failed("Email not verified", f),
    }
}

async fn reset_page(Query(q): Query<TokenQuery>) -> Page {
    match q.token.filter(|t| !t.is_empty()) {
        Some(token) => page(StatusCode::OK, "Choose a new password", &reset_form(&token, None)),
        None => page(
            StatusCode::OK,
            "Reset your password",
            &format!(
                "<form method=\"post\" action=\"{PAGE_RESET_REQUEST}\">\n\
                 <p><label>Email address<br><input type=\"email\" name=\"email\" autocomplete=\"email\" required></label></p>\n\
                 <p><button type=\"submit\">Mail me a reset link</button></p>\n</form>"
            ),
        ),
    }
}

async fn reset_submit(State(s): State<AppState>, Form(f): Form<ResetForm>) -> Page {
    // A refused password does not spend the token: show the form again.
    if let Err(msg) = check_password(&f.password) {
        return page(StatusCode::BAD_REQUEST, "Choose a new password", &reset_form(&f.token, Some(msg)));
    }
    match confirm_reset(&s, &f.token, f.password).await {
        Ok(()) => page(
            StatusCode::OK,
            "Password changed",
            "<p>Password changed. Every device was signed out; log in again from the game.</p>",
        ),
        Err(e) => failed("Password not changed", e),
    }
}

async fn reset_request_submit(State(s): State<AppState>, Form(f): Form<ResetRequestForm>) -> Page {
    match request_reset(&s, &f.email).await {
        Ok(()) => page(
            StatusCode::OK,
            "Check your mail",
            "<p>If an account uses that address, a reset link is on its way.</p>",
        ),
        Err(e) => failed("Reset not sent", e),
    }
}

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route(PAGE_VERIFY, get(verify_page))
        .route(PAGE_RESET, get(reset_page).post(reset_submit))
        .route(PAGE_RESET_REQUEST, post(reset_request_submit))
}
