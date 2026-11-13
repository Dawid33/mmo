//! Play online: signing in, listing the login service's worlds and joining one
//! with a login-issued JWT (R2a). The screens are `ui/login.slint` and
//! `ui/worlds.slint` ([`AppState::Login`], [`AppState::Worlds`]); the
//! frontend turns their buttons into [`LoginIntent`]s, [`start_login`] sends
//! the calls, [`poll_login`] applies the answers ([`LoginSession::apply`], a
//! pure state step the tests drive without a network).
//!
//! The HTTP calls never run on the main thread. Native: one dedicated
//! `login-http` thread with a current-thread tokio runtime, calls in / replies
//! out over crossbeam. Browser: `wasm_bindgen_futures::spawn_local` per call,
//! replies back over the same channel type.
//!
//! Every refresh token the service hands out replaces the previous one, and
//! presenting a replaced one revokes the whole login (the service's reuse
//! detection). Several clients on one machine (two native clients, two
//! browser tabs) share one kept token, so before every call that presents it
//! (refresh, join, logout) the session re-reads the store and adopts what is
//! kept there ([`LoginSession::adopt_kept`]) — another client may have rotated
//! it meanwhile. For the same reason the native client never refreshes at
//! startup: the kept token is used only when the player presses Play online.
//! The browser opens on its login screen, so it still refreshes at startup.
//!
//! Signing in is the identity provider's device flow (RFC 8628, with PKCE;
//! parsing in [`super::oidc`]): Sign in fetches where the service's players
//! sign in (`GET /v1/config`) and starts a device sign-in there
//! ([`LoginCall::DeviceStart`]); its answer becomes the
//! [`LoginSession::pending_device`] (the code the screen shows; native opens
//! the browser at it). [`LoginSession::tick`] then sends one
//! [`LoginCall::DevicePoll`] at a time, each the provider's interval plus
//! [`DEVICE_POLL_MARGIN_SECS`] after the previous one, until the provider hands out an access token,
//! which [`LoginCall::LoginOidc`] trades for the service's refresh token (the
//! same path a refresh takes from there). The polling stops for good on the
//! token, on an expired or refused sign-in, at the deadline, on Cancel or
//! Back, and as soon as the login screen is left; a poll's answer for a
//! sign-in that is no longer the pending one is dropped.
//!
//! Back leaves the login screens even while a call is in flight: the calls
//! are tagged with a generation, Back starts a new one (the screens are idle
//! again), and a reply of an older generation still records its token and
//! account but changes no screen and no line on it.
//!
//! A join rotates the refresh token (kept BEFORE connecting), then hands the
//! session flow a [`PendingJoin`]: native connects to the ticket's QUIC
//! address (resolved on the `login-http` thread), the browser to its
//! WebTransport URL (pinning the ticket's certificate hash when it has one),
//! both with `Credential::Jwt`. When an online session ends the player is
//! back on the world list ([`LoginSession::home`]).
//!
//! Never log a refresh token, an access token, a device code, the PKCE
//! verifier, a ticket or a JWT: nothing here derives `Debug` over them.

#[cfg(not(target_arch = "wasm32"))]
use std::net::SocketAddr;

use bevy::prelude::*;
use crossbeam::channel::{unbounded, Receiver, Sender};
use login_api::{ApiErrorCode, JoinTicket, LoginConfigResponse, LoginResponse, WorldEntry};

use super::oidc::{self, DevicePoll, DeviceStart};
use super::{store, LoginApi, LoginFailure};
use crate::session::{AppState, PendingJoin};

/// The login screen's line when the device sign-in ran out (the provider said
/// so, or its deadline passed) before the player confirmed it.
pub const SIGN_IN_EXPIRED: &str = "Sign-in expired";
/// The line when the player refused the sign-in at the provider.
pub const SIGN_IN_REFUSED: &str = "Sign-in was refused";
/// The line when the provider ended the sign-in for another reason.
pub const SIGN_IN_FAILED: &str = "Sign-in failed \u{2014} try again";
/// Seconds added to the provider's poll interval before each device poll: a
/// poll sent exactly one interval after the previous one can, with network
/// jitter, arrive early and earn a `slow_down`.
pub const DEVICE_POLL_MARGIN_SECS: f64 = 1.0;
/// Whether this platform opens the provider's page itself. The browser does
/// not: the page opens the tab from the button's click (a popup needs the
/// player's gesture).
pub const OPENS_BROWSER: bool = cfg!(not(target_arch = "wasm32"));
/// The world list's line when a ticket has no address this client can use.
pub const NO_ADDRESS_ERROR: &str = "That world cannot be reached from this client";

/// What a frontend button asked of the login service.
#[derive(Message, Clone)]
pub enum LoginIntent {
    /// The main menu's Play online.
    PlayOnline,
    /// The login screen's Sign in: start a device sign-in.
    SignIn,
    /// Open the pending sign-in's page (again).
    OpenBrowser,
    /// Stop waiting for the pending sign-in.
    CancelSignIn,
    JoinWorld(String),
    Logout,
    /// Back on the login or world screen: abandon the calls in flight.
    Back,
}

/// Whether the native client refreshes a kept token at startup. Only the
/// browser does (it opens on the login screen); a native client uses the kept
/// token only on Play online, so two clients started on one machine do not
/// both rotate it at once.
pub const REFRESH_AT_STARTUP: bool = cfg!(target_arch = "wasm32");

/// One call to the login service. Holds secrets: no `Debug`.
pub enum LoginCall {
    /// Where the service's players sign in, then a device sign-in there with
    /// the PKCE challenge of `verifier`.
    DeviceStart { verifier: String },
    /// One poll of the pending device sign-in.
    DevicePoll { provider: LoginConfigResponse, device_code: String, verifier: String },
    /// The provider's access token traded for the service's refresh token.
    LoginOidc { access_token: String },
    /// `silent`: the refresh of a kept token (the browser's startup, or Play
    /// online with a kept token) rather than a sign-in.
    Refresh { token: String, silent: bool },
    Worlds,
    Join { token: String, world: String },
    Logout { token: String },
}

/// A join's answer: the ticket, the rotated refresh token and (native) the
/// ticket's QUIC address resolved off the main thread.
pub struct Joined {
    pub ticket: JoinTicket,
    pub refresh_token: String,
    #[cfg(not(target_arch = "wasm32"))]
    pub quic: Result<SocketAddr, String>,
}

/// One answer of the login service. Holds secrets: no `Debug`.
pub enum LoginReply {
    /// A sign-in or a refresh (`silent`: of a kept token).
    Session { silent: bool, result: Result<LoginResponse, LoginFailure> },
    /// A device sign-in started (`verifier`: the one its call sent the
    /// challenge of), with the provider it runs at.
    DeviceStarted { verifier: String, result: Result<(LoginConfigResponse, DeviceStart), LoginFailure> },
    /// One poll's answer, for the sign-in of `device_code`. `Err`: the poll
    /// did not get through (the sign-in goes on).
    DevicePolled { device_code: String, result: Result<DevicePoll, LoginFailure> },
    Worlds(Result<Vec<WorldEntry>, LoginFailure>),
    Joined(Result<Joined, LoginFailure>),
    /// Logout is fire-and-forget: the local login is gone already.
    LoggedOut,
}

/// Whether `failure` says the refresh token itself is no good (revoked,
/// expired, unknown), as opposed to the service being down or failing.
fn refused_token(failure: &LoginFailure) -> bool {
    matches!(failure, LoginFailure::Api(e) if e.code == ApiErrorCode::Revoked)
}

/// Whether a call's reply counts toward [`LoginSession::busy`] (logout does
/// not: nothing waits for it; nor does a device poll: the sign-in's waiting
/// is [`LoginSession::waiting_for_sign_in`], and its polls come and go).
fn awaited(call: &LoginCall) -> bool {
    !matches!(call, LoginCall::Logout { .. } | LoginCall::DevicePoll { .. })
}

/// A device sign-in started and not finished: the code the player confirms
/// at the provider, and when to poll next. Holds the device code and the
/// PKCE verifier: no `Debug`.
pub struct PendingDevice {
    /// The provider the sign-in runs at.
    pub provider: LoginConfigResponse,
    /// What the screen shows: `start.user_code`, `start.verification_uri`
    /// and `start.verification_uri_complete`.
    pub start: DeviceStart,
    /// The PKCE verifier every poll presents (a secret).
    verifier: String,
    /// When the sign-in runs out ([`LoginSession::tick`]'s clock, seconds).
    pub deadline: f64,
    /// The earliest time of the next poll.
    pub next_poll: f64,
    /// Seconds between two polls (the provider's, longer after `slow_down`).
    pub interval: u64,
    /// A poll is on its way: no second one until it answers.
    polling: bool,
}

/// Runs one call against the service.
pub async fn perform(api: &LoginApi, call: LoginCall) -> LoginReply {
    match call {
        LoginCall::DeviceStart { verifier } => {
            let result = async {
                let provider = api.config().await?;
                let challenge = oidc::pkce_challenge(&verifier);
                let start = api.device_start(&provider.oidc_issuer, &provider.oidc_client_id, &challenge).await?;
                Ok((provider, start))
            }
            .await;
            LoginReply::DeviceStarted { verifier, result }
        }
        LoginCall::DevicePoll { provider, device_code, verifier } => {
            let result =
                api.device_poll(&provider.oidc_issuer, &provider.oidc_client_id, &device_code, &verifier).await;
            LoginReply::DevicePolled { device_code, result }
        }
        LoginCall::LoginOidc { access_token } => {
            LoginReply::Session { silent: false, result: api.login_oidc(&access_token).await }
        }
        LoginCall::Refresh { token, silent } => LoginReply::Session { silent, result: api.refresh(&token).await },
        LoginCall::Worlds => LoginReply::Worlds(api.worlds().await),
        LoginCall::Join { token, world } => LoginReply::Joined(api.join(&token, &world).await.map(|response| {
            #[cfg(not(target_arch = "wasm32"))]
            let quic = resolve_quic(response.ticket.quic.as_deref());
            Joined {
                ticket: response.ticket,
                refresh_token: response.refresh_token,
                #[cfg(not(target_arch = "wasm32"))]
                quic,
            }
        })),
        LoginCall::Logout { token } => {
            let _ = api.logout(&token).await; // errors ignored: the login is gone locally
            LoginReply::LoggedOut
        }
    }
}

/// A ticket's `host:port` as a socket address (a hostname resolved; blocking,
/// so only on the `login-http` thread).
#[cfg(not(target_arch = "wasm32"))]
pub fn resolve_quic(quic: Option<&str>) -> Result<SocketAddr, String> {
    use std::net::ToSocketAddrs;
    let Some(quic) = quic.map(str::trim).filter(|q| !q.is_empty()) else {
        return Err(NO_ADDRESS_ERROR.into());
    };
    match quic.to_socket_addrs() {
        Ok(mut addrs) => addrs.next().ok_or_else(|| format!("{quic} has no address")),
        Err(e) => {
            warn!("[login] cannot resolve the world's address {quic}: {e}");
            Err(format!("Cannot resolve the world's address {quic}"))
        }
    }
}

/// Which batch of calls a reply belongs to: [`LoginSession::back`] starts a
/// new generation, and replies of older ones no longer count as in flight.
pub type Generation = u64;

/// Where calls go and replies come back (each tagged with its generation).
struct LoginWorker {
    calls: CallSink,
    replies: Receiver<(Generation, LoginReply)>,
}

#[cfg(not(target_arch = "wasm32"))]
struct CallSink(Sender<(Generation, LoginCall)>);

#[cfg(target_arch = "wasm32")]
struct CallSink {
    api: LoginApi,
    replies: Sender<(Generation, LoginReply)>,
}

impl LoginWorker {
    /// Native: the `login-http` thread (it ends when the session's sender is
    /// dropped).
    #[cfg(not(target_arch = "wasm32"))]
    fn start(api: LoginApi) -> Self {
        let (call_send, call_recv) = unbounded::<(Generation, LoginCall)>();
        let (reply_send, replies) = unbounded();
        let spawned = std::thread::Builder::new().name("login-http".into()).spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(runtime) => runtime,
                Err(e) => {
                    error!("[login] no runtime for the login client: {e}");
                    return;
                }
            };
            for (generation, call) in call_recv {
                if reply_send.send((generation, runtime.block_on(perform(&api, call)))).is_err() {
                    break;
                }
            }
        });
        if let Err(e) = spawned {
            error!("[login] cannot start the login-http thread: {e}");
        }
        Self { calls: CallSink(call_send), replies }
    }

    #[cfg(target_arch = "wasm32")]
    fn start(api: LoginApi) -> Self {
        let (reply_send, replies) = unbounded();
        Self { calls: CallSink { api, replies: reply_send }, replies }
    }

    /// Sends `call`; `false` if the worker is gone (the reply never comes).
    fn send(&self, generation: Generation, call: LoginCall) -> bool {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.calls.0.send((generation, call)).is_ok()
        }
        #[cfg(target_arch = "wasm32")]
        {
            let (api, replies) = (self.calls.api.clone(), self.calls.replies.clone());
            wasm_bindgen_futures::spawn_local(async move {
                let _ = replies.send((generation, perform(&api, call).await));
            });
            true
        }
    }
}

/// What to do with the kept refresh token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StoreOp {
    Keep,
    /// Replace it (holds the token: compare, never print).
    Save(String),
    Clear,
}

/// What one [`LoginSession::apply`] / [`LoginSession::intent`] asks of the
/// app beyond the session's own fields.
#[must_use]
pub struct Step {
    pub goto: Option<AppState>,
    pub store: StoreOp,
    pub calls: Vec<LoginCall>,
    pub join: Option<PendingJoin>,
    /// Open this page in the system browser (native only, [`OPENS_BROWSER`]).
    pub open: Option<String>,
}

impl Step {
    fn none() -> Self {
        Self { goto: None, store: StoreOp::Keep, calls: Vec::new(), join: None, open: None }
    }
    fn goto(state: AppState) -> Self {
        Self { goto: Some(state), ..Self::none() }
    }
    fn call(call: LoginCall) -> Self {
        Self { calls: vec![call], ..Self::none() }
    }
    /// Opens `url` where this platform opens pages itself, else nothing.
    fn open(url: &str) -> Self {
        Self { open: OPENS_BROWSER.then(|| url.to_string()), ..Self::none() }
    }
}

/// The player's login: the kept refresh token, the signed-in account, the
/// world list and what the login screens show.
///
/// Deliberately NOT registered with `add_session_reset`: the login outlives a
/// world session (an online session ends back on the world list, still
/// signed in). Exists only when a login service is configured
/// ([`super::login_url`]); without it the menu has no Play online.
#[derive(Resource)]
pub struct LoginSession {
    pub api: LoginApi,
    /// The refresh token (a secret: never logged).
    pub refresh: Option<String>,
    /// The signed-in account's email, once a sign-in or refresh succeeded.
    pub account: Option<String>,
    /// The signed-in account's display name ("" = unknown or signed out).
    pub account_name: String,
    pub worlds: Vec<WorldEntry>,
    /// The screens' error line ("" = none).
    pub error: String,
    /// The running world session was joined with a ticket: ending it returns
    /// to the world list.
    pub playing_online: bool,
    /// The device sign-in the player is confirming at the provider, if any.
    pub pending_device: Option<PendingDevice>,
    /// The frame clock (seconds) as of the last [`Self::tick`]: device
    /// sign-in deadlines and poll times count on it.
    clock: f64,
    /// Calls of the current [`Generation`] whose reply has not come back
    /// (the replies come over [`LoginWorker`]'s channel).
    in_flight: u32,
    /// The current generation (bumped by [`Self::back`]).
    generation: Generation,
    worker: Option<LoginWorker>,
}

/// The refresh token to present: the kept one when there is one (another
/// client on this machine may have rotated it since this one last did), else
/// the one in memory.
pub fn adopt(current: Option<String>, kept: Option<String>) -> Option<String> {
    kept.or(current)
}

impl LoginSession {
    /// A session with no worker yet (started on the first call).
    pub fn new(api: LoginApi) -> Self {
        Self {
            api,
            refresh: None,
            account: None,
            account_name: String::new(),
            worlds: Vec::new(),
            error: String::new(),
            playing_online: false,
            pending_device: None,
            clock: 0.0,
            in_flight: 0,
            generation: 0,
            worker: None,
        }
    }

    /// Re-reads what the store keeps ([`adopt`]): call before anything that
    /// presents the refresh token.
    pub fn adopt_kept(&mut self, kept: Option<String>) {
        self.refresh = adopt(self.refresh.take(), kept);
    }

    /// Back on the login screens: the calls in flight are abandoned (their
    /// replies still record a token or an account, see [`Self::receive`]),
    /// the device sign-in is dropped (no more polls) and the screens are
    /// usable again.
    pub fn back(&mut self) {
        self.generation += 1;
        self.in_flight = 0;
        self.pending_device = None;
    }

    /// A request is in flight: the screens' buttons are disabled. A device
    /// sign-in's waiting is not "busy" ([`Self::waiting_for_sign_in`]); its
    /// start and the final token trade are.
    pub fn busy(&self) -> bool {
        self.in_flight > 0
    }

    /// A device sign-in is waiting for the player to confirm it at the
    /// provider ([`Self::pending_device`] holds its code).
    pub fn waiting_for_sign_in(&self) -> bool {
        self.pending_device.is_some()
    }

    /// The pending sign-in's code for the player to type ("" = none).
    pub fn user_code(&self) -> &str {
        self.pending_device.as_ref().map_or("", |p| p.start.user_code.as_str())
    }

    /// The pending sign-in's page with the code filled in ("" = none).
    pub fn verification_uri_complete(&self) -> &str {
        self.pending_device.as_ref().map_or("", |p| p.start.verification_uri_complete.as_str())
    }

    pub fn logged_in(&self) -> bool {
        self.account.is_some()
    }

    /// Where the player lands when a world session ends: the world list after
    /// an online session (still signed in), else the main menu.
    pub fn home(login: Option<&Self>) -> AppState {
        match login {
            Some(login) if login.playing_online && login.logged_in() => AppState::Worlds,
            _ => AppState::MainMenu,
        }
    }

    /// Where Log out lands: the browser's online screen is its front door.
    pub fn logged_out_state() -> AppState {
        if cfg!(target_arch = "wasm32") { AppState::Login } else { AppState::MainMenu }
    }

    /// Forget the login locally.
    fn sign_out(&mut self) {
        self.refresh = None;
        self.account = None;
        self.account_name.clear();
        self.worlds.clear();
        self.playing_online = false;
        self.pending_device = None;
    }

    /// The browser's startup step ([`REFRESH_AT_STARTUP`]): a kept refresh
    /// token is refreshed silently.
    pub fn resume(&mut self, kept: Option<String>) -> Step {
        let Some(token) = kept else { return Step::none() };
        self.refresh = Some(token.clone());
        Step::call(LoginCall::Refresh { token, silent: true })
    }

    /// A button on the login screens (`state`: the screen it was pressed on).
    pub fn intent(&mut self, intent: LoginIntent, state: AppState) -> Step {
        let busy = self.busy();
        match intent {
            LoginIntent::PlayOnline => {
                self.error.clear();
                if self.logged_in() {
                    return Step::goto(AppState::Worlds);
                }
                let mut step = Step::goto(AppState::Login);
                // A kept login (native: never refreshed yet; browser: its
                // startup refresh failed): refresh it now.
                if let (Some(token), false) = (self.refresh.clone(), busy) {
                    step.calls.push(LoginCall::Refresh { token, silent: true });
                }
                step
            }
            LoginIntent::Back => {
                self.back();
                Step::none()
            }
            // Cancel works while the sign-in starts too: like Back, it
            // abandons the call in flight (its answer is then dropped).
            LoginIntent::CancelSignIn => {
                if busy || self.waiting_for_sign_in() {
                    self.back();
                    self.error.clear();
                }
                Step::none()
            }
            LoginIntent::OpenBrowser => match &self.pending_device {
                Some(pending) => Step::open(&pending.start.verification_uri_complete),
                None => Step::none(),
            },
            _ if busy => Step::none(),
            LoginIntent::SignIn => {
                // Signed in already (a Cancel or Back pressed while the token
                // trade was in flight: its reply still signed the player in
                // and left the screen on Login): on to the world list, which
                // fetches itself on entry (`refresh_worlds`).
                if self.logged_in() {
                    return Step::goto(AppState::Worlds);
                }
                if self.waiting_for_sign_in() {
                    return Step::none();
                }
                self.error.clear();
                Step::call(LoginCall::DeviceStart { verifier: oidc::new_pkce_verifier() })
            }
            LoginIntent::JoinWorld(world) => {
                self.error.clear();
                match self.refresh.clone() {
                    Some(token) => Step::call(LoginCall::Join { token, world }),
                    None => Step::goto(AppState::Login),
                }
            }
            LoginIntent::Logout => {
                let token = self.refresh.clone();
                self.sign_out();
                self.error.clear();
                let mut step = Step::goto(Self::logged_out_state());
                step.store = StoreOp::Clear;
                if state == AppState::Login {
                    step.goto = None;
                }
                step.calls.extend(token.map(|token| LoginCall::Logout { token }));
                step
            }
        }
    }

    /// One reply of `generation`, on screen `state`. A reply of an older
    /// generation (sent before a Back) no longer counts as in flight and is
    /// applied as if the player had left the login screens: its token and
    /// account are recorded, but it changes no screen, starts no session and
    /// leaves the screens' lines alone.
    pub fn receive(&mut self, generation: Generation, reply: LoginReply, state: AppState) -> Step {
        if generation != self.generation {
            let line = std::mem::take(&mut self.error);
            let step = self.apply(reply, AppState::MainMenu);
            self.error = line;
            return step;
        }
        if !matches!(reply, LoginReply::LoggedOut | LoginReply::DevicePolled { .. }) {
            self.in_flight = self.in_flight.saturating_sub(1);
        }
        self.apply(reply, state)
    }

    /// The login worker is gone (its thread died): nothing in flight will
    /// answer. The next call starts a new worker.
    fn worker_lost(&mut self) {
        self.worker = None;
        self.back();
        self.error = LoginFailure::Network("the login client stopped".into()).to_string();
    }

    /// One reply of the service, on screen `state`. A reply changes the screen
    /// only while the player is on the login screens: one that lands after
    /// they left (Back, then a single-player start) records what it says — the
    /// account, a sign-out — but never pulls them out of a session.
    pub fn apply(&mut self, reply: LoginReply, state: AppState) -> Step {
        let mut step = self.apply_reply(reply, state);
        if !matches!(state, AppState::Login | AppState::Worlds) {
            step.goto = None;
        }
        step
    }

    fn apply_reply(&mut self, reply: LoginReply, state: AppState) -> Step {
        match reply {
            LoginReply::Session { silent, result: Ok(response) } => {
                self.refresh = Some(response.refresh_token.clone());
                self.account = Some(response.account_email);
                self.account_name = response.account_name;
                self.error.clear();
                let mut step = Step::call(LoginCall::Worlds);
                step.store = StoreOp::Save(response.refresh_token);
                // A silent refresh moves on only from the login screen (the
                // browser's start); native stays on its menu.
                if state == AppState::Login || !silent {
                    step.goto = Some(AppState::Worlds);
                }
                step
            }
            LoginReply::Session { silent, result: Err(failure) } => {
                self.account = None;
                self.account_name.clear();
                self.error = failure.to_string();
                let mut step = Step::none();
                // The service refused the kept token: forget it. Unreachable,
                // or a server-side problem: keep it for the next try.
                if silent && refused_token(&failure) {
                    self.refresh = None;
                    step.store = StoreOp::Clear;
                }
                step
            }
            LoginReply::DeviceStarted { verifier, result } => self.device_started(verifier, result, state),
            LoginReply::DevicePolled { device_code, result } => self.device_polled(&device_code, result, state),
            LoginReply::Worlds(Ok(worlds)) => {
                self.worlds = worlds;
                Step::none()
            }
            LoginReply::Worlds(Err(failure)) => {
                self.error = failure.to_string();
                Step::none()
            }
            LoginReply::Joined(Ok(joined)) => self.joined(joined, state),
            LoginReply::Joined(Err(failure)) => {
                self.error = failure.to_string();
                if matches!(&failure, LoginFailure::Api(e) if e.code == ApiErrorCode::Revoked) {
                    self.sign_out();
                    return Step { goto: Some(AppState::Login), store: StoreOp::Clear, ..Step::none() };
                }
                Step::none()
            }
            LoginReply::LoggedOut => Step::none(),
        }
    }

    /// A device sign-in's start: it becomes the pending one (from the login
    /// screen only — an answer that lands after the player left, or of an
    /// abandoned generation, starts nothing).
    fn device_started(
        &mut self,
        verifier: String,
        result: Result<(LoginConfigResponse, DeviceStart), LoginFailure>,
        state: AppState,
    ) -> Step {
        let (provider, start) = match result {
            Ok(started) => started,
            Err(failure) => {
                warn!("[login] cannot start a sign-in: {failure:?}");
                self.error = failure.to_string();
                return Step::none();
            }
        };
        if state != AppState::Login {
            return Step::none();
        }
        let interval = start.interval.max(1);
        let step = Step::open(&start.verification_uri_complete);
        self.pending_device = Some(PendingDevice {
            deadline: self.clock + start.expires_in as f64,
            next_poll: self.clock + interval as f64 + DEVICE_POLL_MARGIN_SECS,
            interval,
            provider,
            start,
            verifier,
            polling: false,
        });
        self.error.clear();
        step
    }

    /// One poll's answer. Dropped unless it is for the pending sign-in (one
    /// cancelled, expired or replaced since is gone) and the player is on the
    /// login screen.
    fn device_polled(&mut self, device_code: &str, result: Result<DevicePoll, LoginFailure>, state: AppState) -> Step {
        if state != AppState::Login {
            return Step::none();
        }
        let Some(pending) = self.pending_device.as_mut().filter(|p| p.start.device_code == device_code) else {
            return Step::none();
        };
        pending.polling = false;
        let poll = match result {
            Ok(poll) => poll,
            // Did not get through: poll again at the next interval, until the
            // deadline.
            Err(failure) => {
                warn!("[login] a sign-in poll failed, polling again: {failure:?}");
                return Step::none();
            }
        };
        let interval = oidc::next_interval(pending.interval, &poll);
        // The next poll was due one interval (plus the margin) after this one
        // was sent; a slow_down lengthens that wait as well as every later one.
        pending.next_poll += interval.saturating_sub(pending.interval) as f64;
        pending.interval = interval;
        match poll {
            DevicePoll::Pending | DevicePoll::SlowDown => Step::none(),
            DevicePoll::Token(access_token) => {
                self.pending_device = None;
                Step::call(LoginCall::LoginOidc { access_token })
            }
            DevicePoll::Expired => self.end_sign_in(SIGN_IN_EXPIRED),
            DevicePoll::Denied => self.end_sign_in(SIGN_IN_REFUSED),
            DevicePoll::Failed(why) => {
                warn!("[login] the identity provider ended the sign-in: {why}");
                self.end_sign_in(SIGN_IN_FAILED)
            }
        }
    }

    /// The pending sign-in is over without a login: `line` on the screen.
    fn end_sign_in(&mut self, line: &str) -> Step {
        self.pending_device = None;
        self.error = line.into();
        Step::none()
    }

    /// Every frame, with the frame clock `now` (seconds) on screen `state`:
    /// the pending device sign-in's next poll when it is due and none is on
    /// its way. Off the login screen the sign-in is dropped; past its
    /// deadline it ends as [`SIGN_IN_EXPIRED`].
    pub fn tick(&mut self, now: f64, state: AppState) -> Step {
        self.clock = now;
        let Some(pending) = self.pending_device.as_mut() else { return Step::none() };
        if state != AppState::Login {
            self.pending_device = None;
            return Step::none();
        }
        if now >= pending.deadline {
            return self.end_sign_in(SIGN_IN_EXPIRED);
        }
        if pending.polling || now < pending.next_poll {
            return Step::none();
        }
        pending.polling = true;
        pending.next_poll = now + pending.interval as f64 + DEVICE_POLL_MARGIN_SECS;
        Step::call(LoginCall::DevicePoll {
            provider: pending.provider.clone(),
            device_code: pending.start.device_code.clone(),
            verifier: pending.verifier.clone(),
        })
    }

    /// A ticket: keep the rotated refresh token first, then start the session
    /// if this platform has an address for it.
    fn joined(&mut self, joined: Joined, state: AppState) -> Step {
        self.refresh = Some(joined.refresh_token.clone());
        let mut step = Step::none();
        step.store = StoreOp::Save(joined.refresh_token);
        if state != AppState::Worlds {
            return step; // the player left the list meanwhile
        }
        #[cfg(not(target_arch = "wasm32"))]
        let pending = joined.quic.map(|addr| PendingJoin { ticket: joined.ticket, addr });
        #[cfg(target_arch = "wasm32")]
        let pending = match joined.ticket.webtransport.as_deref().map(str::trim) {
            Some(url) if !url.is_empty() => Ok(PendingJoin { ticket: joined.ticket }),
            _ => Err(NO_ADDRESS_ERROR.to_string()),
        };
        match pending {
            Ok(pending) => {
                self.playing_online = true;
                step.join = Some(pending);
                step.goto = Some(AppState::Starting);
            }
            Err(why) => self.error = why,
        }
        step
    }

    /// Sends `calls` (starting the worker on first use).
    fn send(&mut self, calls: Vec<LoginCall>) {
        for call in calls {
            let worker = self.worker.get_or_insert_with(|| LoginWorker::start(self.api.clone()));
            let counted = awaited(&call);
            if worker.send(self.generation, call) {
                self.in_flight += u32::from(counted);
            } else {
                self.error = LoginFailure::Network("the login client stopped".into()).to_string();
                // A poll that was never sent never answers: drop the sign-in
                // rather than wait for it.
                self.pending_device = None;
            }
        }
    }
}

/// Carries out a step's effects outside the session: the kept token, the
/// pending join, the screen.
fn perform_step(step: Step, login: &mut LoginSession, commands: &mut Commands, next: &mut NextState<AppState>) {
    match &step.store {
        StoreOp::Keep => {}
        StoreOp::Save(token) => store::save_refresh(token),
        StoreOp::Clear => store::clear_refresh(),
    }
    login.send(step.calls);
    if let Some(join) = step.join {
        commands.insert_resource(join);
    }
    if let Some(url) = step.open {
        // The URL was checked (http / https) when the answer was parsed.
        open_browser(&url);
    }
    if let Some(state) = step.goto {
        next.set(state);
    }
}

/// Opens the provider's sign-in page in the system browser (the URL carries
/// the user code: not logged).
#[cfg(not(target_arch = "wasm32"))]
fn open_browser(url: &str) {
    if let Err(e) = open::that_detached(url) {
        warn!("[login] cannot open the browser for the sign-in ({:?})", e.kind());
    }
}

/// The browser never asks ([`OPENS_BROWSER`]): its page opens the tab.
#[cfg(target_arch = "wasm32")]
fn open_browser(_url: &str) {}

/// `Startup`: with a login service configured, the [`LoginSession`]; in the
/// browser ([`REFRESH_AT_STARTUP`]) a kept refresh token is refreshed
/// silently.
pub fn setup_login(mut commands: Commands) {
    let Some(url) = super::login_url() else { return };
    info!("[login] login service at {url}");
    let mut login = LoginSession::new(LoginApi::new(url));
    if REFRESH_AT_STARTUP {
        let step = login.resume(store::load_refresh());
        login.send(step.calls);
    }
    commands.insert_resource(login);
}

/// Whether `intent` presents the refresh token (so the kept one is re-read
/// first).
fn presents_token(intent: &LoginIntent) -> bool {
    matches!(intent, LoginIntent::PlayOnline | LoginIntent::JoinWorld(_) | LoginIntent::Logout)
}

/// The frontend's [`LoginIntent`]s, sent.
pub fn start_login(
    mut intents: MessageReader<LoginIntent>,
    login: Option<ResMut<LoginSession>>,
    state: Res<State<AppState>>,
    mut commands: Commands,
    mut next: ResMut<NextState<AppState>>,
) {
    let Some(mut login) = login else {
        intents.clear();
        return;
    };
    for intent in intents.read() {
        if presents_token(intent) {
            login.adopt_kept(store::load_refresh());
        }
        let step = login.intent(intent.clone(), *state.get());
        perform_step(step, &mut login, &mut commands, &mut next);
    }
}

/// The service's answers, applied; then the device sign-in's next poll
/// ([`LoginSession::tick`], on the real-time clock: it runs on native and in
/// the browser alike, and never pauses).
pub fn poll_login(
    login: Option<ResMut<LoginSession>>,
    state: Res<State<AppState>>,
    time: Res<Time<Real>>,
    mut commands: Commands,
    mut next: ResMut<NextState<AppState>>,
) {
    let Some(mut login) = login else { return };
    let now = time.elapsed_secs_f64();
    login.clock = now;
    let (replies, lost): (Vec<(Generation, LoginReply)>, bool) = match &login.worker {
        Some(worker) => {
            let replies = worker.replies.try_iter().collect();
            let lost = matches!(worker.replies.try_recv(), Err(crossbeam::channel::TryRecvError::Disconnected));
            (replies, lost)
        }
        None => (Vec::new(), false),
    };
    for (generation, reply) in replies {
        let step = login.receive(generation, reply, *state.get());
        perform_step(step, &mut login, &mut commands, &mut next);
    }
    if lost {
        error!("[login] the login-http worker stopped");
        login.worker_lost();
    }
    let step = login.tick(now, *state.get());
    perform_step(step, &mut login, &mut commands, &mut next);
}

/// `OnEnter(Worlds)`: back on the list (an online session over, or from the
/// menu): fetch the list again.
pub fn refresh_worlds(login: Option<ResMut<LoginSession>>) {
    let Some(mut login) = login else { return };
    login.playing_online = false;
    if login.logged_in() && !login.busy() {
        login.send(vec![LoginCall::Worlds]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use login_api::{ApiError, WorldStatus};

    fn session() -> LoginSession {
        LoginSession::new(LoginApi::new("http://127.0.0.1:1"))
    }

    fn logged_in(email: &str, token: &str) -> LoginReply {
        LoginReply::Session {
            silent: false,
            result: Ok(LoginResponse {
                refresh_token: token.into(),
                account_email: email.into(),
                email_verified: false,
                account_name: String::new(),
            }),
        }
    }

    fn api(code: ApiErrorCode) -> LoginFailure {
        LoginFailure::Api(ApiError { code, message: String::new() })
    }

    fn ticket(quic: Option<&str>, webtransport: Option<&str>) -> JoinTicket {
        JoinTicket {
            world: "alpha".into(),
            jwt: "header.claims.sig".into(),
            quic: quic.map(Into::into),
            webtransport: webtransport.map(Into::into),
            cert_hash: None,
            expires_in_secs: 300,
        }
    }

    fn joined(ticket: JoinTicket) -> Joined {
        #[cfg(not(target_arch = "wasm32"))]
        let quic = resolve_quic(ticket.quic.as_deref());
        Joined {
            ticket,
            refresh_token: "rotated".into(),
            #[cfg(not(target_arch = "wasm32"))]
            quic,
        }
    }

    #[test]
    fn login_keeps_the_token_lists_worlds_and_shows_the_list() {
        let mut login = session();
        let step = login.apply(logged_in("a@b.c", "r1"), AppState::Login);
        assert_eq!(step.goto, Some(AppState::Worlds));
        assert_eq!(step.store, StoreOp::Save("r1".into()));
        assert!(matches!(step.calls.as_slice(), [LoginCall::Worlds]));
        assert_eq!(login.account.as_deref(), Some("a@b.c"));
    }

    #[test]
    fn a_kept_token_refreshes_silently() {
        let mut login = session();
        let step = login.resume(Some("kept".into()));
        assert!(matches!(step.calls.as_slice(), [LoginCall::Refresh { silent: true, .. }]));
        // Native stays on its menu, the browser's login screen moves on.
        let ok = |token: &str| LoginReply::Session {
            silent: true,
            result: Ok(LoginResponse { refresh_token: token.into(), account_email: "a@b.c".into(), email_verified: true, account_name: String::new() }),
        };
        assert_eq!(login.apply(ok("r2"), AppState::MainMenu).goto, None);
        assert_eq!(session().apply(ok("r2"), AppState::Login).goto, Some(AppState::Worlds));
        // Play online then goes straight to the list.
        assert_eq!(login.intent(LoginIntent::PlayOnline, AppState::MainMenu).goto, Some(AppState::Worlds));
    }

    #[test]
    fn a_refused_kept_token_is_forgotten_an_unreachable_one_kept() {
        let mut login = session();
        let _ = login.resume(Some("kept".into()));
        let refused = LoginReply::Session { silent: true, result: Err(api(ApiErrorCode::Revoked)) };
        assert_eq!(login.apply(refused, AppState::Login).store, StoreOp::Clear);
        assert_eq!(login.refresh, None);

        let mut login = session();
        let _ = login.resume(Some("kept".into()));
        let offline = LoginReply::Session { silent: true, result: Err(LoginFailure::Network("down".into())) };
        assert_eq!(login.apply(offline, AppState::Login).store, StoreOp::Keep);
        assert_eq!(login.error, "Can't reach the login server");
        // Play online retries it.
        let step = login.intent(LoginIntent::PlayOnline, AppState::MainMenu);
        assert_eq!(step.goto, Some(AppState::Login));
        assert!(matches!(step.calls.as_slice(), [LoginCall::Refresh { .. }]));
    }

    #[test]
    fn only_a_refused_kept_token_is_forgotten() {
        for (failure, cleared) in [
            (api(ApiErrorCode::Revoked), true),
            (api(ApiErrorCode::Internal), false),
            (api(ApiErrorCode::IdentityRefused), false),
            (LoginFailure::Protocol("502".into()), false),
            (LoginFailure::Network("down".into()), false),
        ] {
            let mut login = session();
            let _ = login.resume(Some("kept".into()));
            let label = format!("{failure:?}");
            let step = login.apply(LoginReply::Session { silent: true, result: Err(failure) }, AppState::Login);
            assert_eq!(step.store == StoreOp::Clear, cleared, "{label}");
            assert_eq!(login.refresh.is_none(), cleared, "{label}");
        }
    }

    /// A reply that lands after the player left the login screens (Back,
    /// then a single-player start) never moves them.
    #[test]
    fn replies_change_the_screen_only_on_the_login_screens() {
        let mut login = session();
        let step = login.apply(logged_in("a@b.c", "r1"), AppState::Starting);
        assert_eq!(step.goto, None, "a late login does not leave a starting session");
        assert_eq!(step.store, StoreOp::Save("r1".into()));
        assert!(login.logged_in(), "the account is still recorded");
        assert_eq!(login.apply(logged_in("a@b.c", "r2"), AppState::MainMenu).goto, None);

        let step = login.apply(LoginReply::Joined(Err(api(ApiErrorCode::Revoked))), AppState::InWorld);
        assert_eq!(step.goto, None, "a late revoke does not leave the world");
        assert_eq!(step.store, StoreOp::Clear, "but the token is cleared");
        assert!(!login.logged_in() && login.refresh.is_none());
    }

    #[test]
    fn the_kept_token_is_adopted_when_it_differs() {
        assert_eq!(adopt(Some("mine".into()), Some("rotated-elsewhere".into())).as_deref(), Some("rotated-elsewhere"));
        assert_eq!(adopt(Some("same".into()), Some("same".into())).as_deref(), Some("same"));
        assert_eq!(adopt(Some("mine".into()), None).as_deref(), Some("mine"), "nothing kept: keep ours");
        assert_eq!(adopt(None, Some("kept".into())).as_deref(), Some("kept"));
        assert_eq!(adopt(None, None), None);

        // A join presents the adopted token, not the stale one in memory.
        let mut login = session();
        let _ = login.apply(logged_in("a@b.c", "r1"), AppState::Login);
        login.adopt_kept(Some("r2".into()));
        let step = login.intent(LoginIntent::JoinWorld("alpha".into()), AppState::Worlds);
        assert!(matches!(step.calls.as_slice(), [LoginCall::Join { token, .. }] if token == "r2"));
    }

    /// Native: no refresh at startup; Play online with a kept token refreshes
    /// it, and its answer moves on from the login screen to the list.
    #[test]
    fn play_online_refreshes_a_kept_token_then_shows_the_list() {
        let mut login = session();
        login.adopt_kept(Some("kept".into()));
        let step = login.intent(LoginIntent::PlayOnline, AppState::MainMenu);
        assert_eq!(step.goto, Some(AppState::Login));
        assert!(matches!(step.calls.as_slice(), [LoginCall::Refresh { token, silent: true }] if token == "kept"));
        let ok = LoginReply::Session {
            silent: true,
            result: Ok(LoginResponse { refresh_token: "r2".into(), account_email: "a@b.c".into(), email_verified: true, account_name: String::new() }),
        };
        let step = login.receive(0, ok, AppState::Login);
        assert_eq!(step.goto, Some(AppState::Worlds));
        assert_eq!(step.store, StoreOp::Save("r2".into()));
        // Refused instead: the login screen stays, the token is forgotten.
        let mut login = session();
        login.adopt_kept(Some("kept".into()));
        let _ = login.intent(LoginIntent::PlayOnline, AppState::MainMenu);
        let refused = LoginReply::Session { silent: true, result: Err(api(ApiErrorCode::Revoked)) };
        let step = login.receive(0, refused, AppState::Login);
        assert_eq!((step.goto, step.store), (None, StoreOp::Clear));
        assert_eq!(REFRESH_AT_STARTUP, cfg!(target_arch = "wasm32"), "only the browser refreshes at startup");
    }

    /// Back while a call hangs: the screens are idle again at once; the late
    /// reply records its token but moves nothing and does not underflow the
    /// counter, and the next call counts normally.
    #[test]
    fn back_abandons_calls_in_flight() {
        let mut login = session();
        login.in_flight = 1; // a login call of generation 0 hangs
        assert!(login.busy());
        let _ = login.intent(LoginIntent::Back, AppState::Login);
        assert!(!login.busy(), "Back makes the screens usable");

        // The player is back on the login screen (pressed Play online again)
        // when the old reply lands.
        login.error = "typed-screen line".into();
        let step = login.receive(0, logged_in("a@b.c", "late"), AppState::Login);
        assert_eq!(step.goto, None, "a stale reply changes no screen");
        assert!(step.join.is_none());
        assert_eq!(step.store, StoreOp::Save("late".into()), "but its token is kept");
        assert!(login.logged_in(), "and its account recorded");
        assert_eq!(login.error, "typed-screen line", "and the screen's lines are left alone");
        assert_eq!(login.in_flight, 0, "no underflow");

        // A stale join never starts a session.
        let stale_join = LoginReply::Joined(Ok(joined(ticket(Some("127.0.0.1:6466"), Some("https://127.0.0.1:6467/")))));
        let step = login.receive(0, stale_join, AppState::Worlds);
        assert!(step.join.is_none() && step.goto.is_none());
        assert!(!login.playing_online);

        // Calls of the new generation count and settle as usual.
        login.in_flight = 1;
        let _ = login.receive(1, LoginReply::Worlds(Ok(Vec::new())), AppState::Worlds);
        assert!(!login.busy());
        let _ = login.receive(1, LoginReply::Worlds(Ok(Vec::new())), AppState::Worlds);
        assert_eq!(login.in_flight, 0, "an extra reply saturates");
    }

    #[test]
    fn a_lost_worker_frees_the_screens() {
        let mut login = session();
        login.in_flight = 2;
        login.worker_lost();
        assert!(!login.busy());
        assert!(login.worker.is_none());
        assert_eq!(login.error, "Can't reach the login server");
    }

    #[test]
    fn nothing_is_sent_while_busy() {
        let mut login = session();
        login.in_flight = 1;
        let step = login.intent(LoginIntent::SignIn, AppState::Login);
        assert!(step.calls.is_empty());
    }

    const COMPLETE_URI: &str = "https://auth.test/realms/mmo/device?user_code=WDJB-MJHT";

    fn provider() -> LoginConfigResponse {
        LoginConfigResponse { oidc_issuer: "https://auth.test/realms/mmo".into(), oidc_client_id: "game".into() }
    }

    fn device_start(code: &str) -> DeviceStart {
        DeviceStart {
            device_code: code.into(),
            user_code: "WDJB-MJHT".into(),
            verification_uri: "https://auth.test/realms/mmo/device".into(),
            verification_uri_complete: COMPLETE_URI.into(),
            expires_in: 600,
            interval: 5,
        }
    }

    /// Sign in on the login screen at `now`, and the start's answer (device
    /// code `code`, 600 s, every 5 s) as the worker would hand it back.
    fn sign_in(login: &mut LoginSession, code: &str, now: f64) -> Step {
        let step = login.intent(LoginIntent::SignIn, AppState::Login);
        assert!(matches!(step.calls.as_slice(), [LoginCall::DeviceStart { .. }]), "Sign in starts a device sign-in");
        login.in_flight += 1; // what `send` counts for it
        assert!(login.busy());
        let _ = login.tick(now, AppState::Login);
        let started = LoginReply::DeviceStarted { verifier: "verifier".into(), result: Ok((provider(), device_start(code))) };
        login.receive(login.generation, started, AppState::Login)
    }

    fn polled(code: &str, poll: DevicePoll) -> LoginReply {
        LoginReply::DevicePolled { device_code: code.into(), result: Ok(poll) }
    }

    /// How many polls `tick` sends (and that they are for `code`).
    fn polls(step: &Step, code: &str) -> usize {
        step.calls
            .iter()
            .map(|call| match call {
                LoginCall::DevicePoll { device_code, verifier, provider } => {
                    assert_eq!(device_code, code);
                    assert_eq!(verifier, "verifier", "every poll presents the start's verifier");
                    assert_eq!(provider.oidc_client_id, "game");
                }
                _ => panic!("tick sends only polls"),
            })
            .count()
    }

    /// Review Focus 4: the player never confirms. The provider's
    /// `expired_token` and the deadline both end the sign-in with
    /// "Sign-in expired", leave an idle login screen and stop the polling;
    /// a new sign-in can start.
    #[test]
    fn expired_device_clears_pending_and_reports() {
        let mut login = session();
        let step = sign_in(&mut login, "dev-1", 100.0);
        assert_eq!(step.open.as_deref(), OPENS_BROWSER.then_some(COMPLETE_URI), "native opens the browser");
        assert!(login.waiting_for_sign_in() && !login.busy());
        assert_eq!(login.user_code(), "WDJB-MJHT");
        assert_eq!(login.verification_uri_complete(), COMPLETE_URI);

        // Due the interval (5 s) plus DEVICE_POLL_MARGIN_SECS (1 s) after the start.
        assert_eq!(polls(&login.tick(104.0, AppState::Login), "dev-1"), 0, "not before the interval");
        assert_eq!(polls(&login.tick(105.0, AppState::Login), "dev-1"), 0, "not before the margin");
        assert_eq!(polls(&login.tick(106.0, AppState::Login), "dev-1"), 1);
        assert_eq!(polls(&login.tick(112.0, AppState::Login), "dev-1"), 0, "at most one poll on its way");
        let _ = login.receive(login.generation, polled("dev-1", DevicePoll::Pending), AppState::Login);
        assert!(login.waiting_for_sign_in() && login.error.is_empty());
        assert_eq!(polls(&login.tick(111.0, AppState::Login), "dev-1"), 0, "not before the interval and margin");
        assert_eq!(polls(&login.tick(112.0, AppState::Login), "dev-1"), 1, "due an interval and margin after the last poll");

        let step = login.receive(login.generation, polled("dev-1", DevicePoll::Expired), AppState::Login);
        assert!(step.calls.is_empty() && step.goto.is_none());
        assert!(!login.waiting_for_sign_in() && !login.busy(), "an idle login screen");
        assert_eq!(login.error, SIGN_IN_EXPIRED);
        assert_eq!(polls(&login.tick(500.0, AppState::Login), "dev-1"), 0, "nothing polls any more");

        // The deadline, while polls keep failing to get through.
        let mut login = session();
        let _ = sign_in(&mut login, "dev-2", 0.0);
        assert_eq!(polls(&login.tick(6.0, AppState::Login), "dev-2"), 1);
        let offline = LoginReply::DevicePolled { device_code: "dev-2".into(), result: Err(LoginFailure::Network("down".into())) };
        let _ = login.receive(login.generation, offline, AppState::Login);
        assert!(login.waiting_for_sign_in(), "a poll that did not get through does not end the sign-in");
        assert_eq!(polls(&login.tick(11.0, AppState::Login), "dev-2"), 0);
        assert_eq!(polls(&login.tick(12.0, AppState::Login), "dev-2"), 1, "it polls again at the next interval");
        let step = login.tick(600.0, AppState::Login);
        assert!(step.calls.is_empty());
        assert!(!login.waiting_for_sign_in());
        assert_eq!(login.error, SIGN_IN_EXPIRED);
        // The last poll's late answer changes nothing.
        let step = login.receive(login.generation, polled("dev-2", DevicePoll::Pending), AppState::Login);
        assert!(step.calls.is_empty() && !login.waiting_for_sign_in());
        assert_eq!(polls(&login.tick(700.0, AppState::Login), "dev-2"), 0);
        // A new sign-in can start.
        let step = login.intent(LoginIntent::SignIn, AppState::Login);
        assert!(matches!(step.calls.as_slice(), [LoginCall::DeviceStart { .. }]));

        // Refused at the provider.
        let mut login = session();
        let _ = sign_in(&mut login, "dev-3", 0.0);
        assert_eq!(polls(&login.tick(6.0, AppState::Login), "dev-3"), 1);
        let _ = login.receive(login.generation, polled("dev-3", DevicePoll::Denied), AppState::Login);
        assert!(!login.waiting_for_sign_in());
        assert_eq!(login.error, SIGN_IN_REFUSED);
    }

    /// Review Focus 4: Cancel, Back, leaving the login screen and an answer
    /// for a sign-in that is no longer the pending one all stop the polling.
    #[test]
    fn cancel_stops_polling() {
        let mut login = session();
        let _ = sign_in(&mut login, "dev-1", 0.0);
        assert_eq!(polls(&login.tick(6.0, AppState::Login), "dev-1"), 1);
        let _ = login.intent(LoginIntent::CancelSignIn, AppState::Login);
        assert!(!login.waiting_for_sign_in() && !login.busy() && login.error.is_empty());
        assert_eq!(polls(&login.tick(100.0, AppState::Login), "dev-1"), 0, "no poll after Cancel");
        // The poll on its way answers with a token after all: dropped (an
        // older generation, and no pending sign-in of its device code).
        let step = login.receive(0, polled("dev-1", DevicePoll::Token("access".into())), AppState::Login);
        assert!(step.calls.is_empty() && step.goto.is_none());
        assert!(!login.logged_in() && !login.waiting_for_sign_in());

        // Cancel while the start is on its way: its answer starts nothing.
        let mut login = session();
        let _ = login.intent(LoginIntent::SignIn, AppState::Login);
        login.in_flight += 1;
        let _ = login.intent(LoginIntent::CancelSignIn, AppState::Login);
        assert!(!login.busy());
        let late = LoginReply::DeviceStarted { verifier: "verifier".into(), result: Ok((provider(), device_start("dev-1"))) };
        let step = login.receive(0, late, AppState::Login);
        assert!(step.open.is_none() && !login.waiting_for_sign_in());
        assert_eq!(polls(&login.tick(100.0, AppState::Login), "dev-1"), 0);

        // Back.
        let mut login = session();
        let _ = sign_in(&mut login, "dev-1", 0.0);
        let _ = login.intent(LoginIntent::Back, AppState::Login);
        assert!(!login.waiting_for_sign_in());
        assert_eq!(polls(&login.tick(100.0, AppState::MainMenu), "dev-1"), 0);

        // Leaving the login screen any other way: the next tick drops it.
        let mut login = session();
        let _ = sign_in(&mut login, "dev-1", 0.0);
        assert_eq!(polls(&login.tick(5.0, AppState::Worlds), "dev-1"), 0);
        assert!(!login.waiting_for_sign_in());

        // An answer for a replaced sign-in (another device code) is dropped.
        let mut login = session();
        let _ = sign_in(&mut login, "dev-1", 0.0);
        let _ = login.intent(LoginIntent::CancelSignIn, AppState::Login);
        let _ = sign_in(&mut login, "dev-2", 10.0);
        let step = login.receive(login.generation, polled("dev-1", DevicePoll::Denied), AppState::Login);
        assert!(step.calls.is_empty());
        assert!(login.waiting_for_sign_in() && login.error.is_empty(), "dev-2 is still pending");
        assert_eq!(polls(&login.tick(16.0, AppState::Login), "dev-2"), 1);
    }

    #[test]
    fn slow_down_lengthens_the_interval() {
        let mut login = session();
        let _ = sign_in(&mut login, "dev-1", 0.0);
        // Every wait is the interval plus DEVICE_POLL_MARGIN_SECS (1 s).
        assert_eq!(polls(&login.tick(6.0, AppState::Login), "dev-1"), 1);
        let _ = login.receive(login.generation, polled("dev-1", DevicePoll::SlowDown), AppState::Login);
        assert_eq!(login.pending_device.as_ref().map(|p| p.interval), Some(10));
        assert_eq!(polls(&login.tick(16.0, AppState::Login), "dev-1"), 0);
        assert_eq!(polls(&login.tick(17.0, AppState::Login), "dev-1"), 1);
        let _ = login.receive(login.generation, polled("dev-1", DevicePoll::Pending), AppState::Login);
        assert_eq!(polls(&login.tick(27.0, AppState::Login), "dev-1"), 0);
        assert_eq!(polls(&login.tick(28.0, AppState::Login), "dev-1"), 1);
    }

    /// The token is traded for the service's refresh token, and from there
    /// it is the usual login: kept, the world list fetched and shown.
    #[test]
    fn a_device_token_signs_in() {
        let mut login = session();
        let _ = sign_in(&mut login, "dev-1", 0.0);
        let _ = login.intent(LoginIntent::SignIn, AppState::Login);
        assert_eq!(polls(&login.tick(6.0, AppState::Login), "dev-1"), 1, "a second Sign in starts nothing");
        let step = login.receive(login.generation, polled("dev-1", DevicePoll::Token("access".into())), AppState::Login);
        assert!(matches!(step.calls.as_slice(), [LoginCall::LoginOidc { access_token }] if access_token == "access"));
        assert!(!login.waiting_for_sign_in());
        login.in_flight += 1;
        let ok = LoginReply::Session {
            silent: false,
            result: Ok(LoginResponse {
                refresh_token: "r1".into(),
                account_email: "a@b.c".into(),
                email_verified: true,
                account_name: "Ada".into(),
            }),
        };
        let step = login.receive(login.generation, ok, AppState::Login);
        assert_eq!(step.goto, Some(AppState::Worlds));
        assert_eq!(step.store, StoreOp::Save("r1".into()));
        assert!(matches!(step.calls.as_slice(), [LoginCall::Worlds]));
        assert_eq!(login.account_name, "Ada");
        assert!(login.logged_in() && !login.busy());
    }

    #[test]
    fn sign_in_while_logged_in_goes_to_the_world_list() {
        let mut login = session();
        let _ = login.apply(logged_in("a@b.c", "r1"), AppState::Login);
        assert!(login.logged_in());
        let step = login.intent(LoginIntent::SignIn, AppState::Login);
        assert_eq!(step.goto, Some(AppState::Worlds));
        assert!(step.calls.is_empty(), "no new sign-in starts");
    }

    #[test]
    fn open_browser_reopens_the_pending_page() {
        let mut login = session();
        assert!(login.intent(LoginIntent::OpenBrowser, AppState::Login).open.is_none(), "nothing pending");
        let _ = sign_in(&mut login, "dev-1", 0.0);
        let step = login.intent(LoginIntent::OpenBrowser, AppState::Login);
        assert_eq!(step.open.as_deref(), OPENS_BROWSER.then_some(COMPLETE_URI));
        assert!(step.calls.is_empty());
    }

    #[test]
    fn a_failed_start_is_an_error_line() {
        let mut login = session();
        let _ = login.intent(LoginIntent::SignIn, AppState::Login);
        login.in_flight += 1;
        let none = LoginReply::DeviceStarted { verifier: "verifier".into(), result: Err(LoginFailure::NoSignIn) };
        let step = login.receive(login.generation, none, AppState::Login);
        assert!(step.open.is_none() && !login.waiting_for_sign_in() && !login.busy());
        assert_eq!(login.error, super::super::api::NO_SIGN_IN);
    }

    #[test]
    fn join_rotates_the_token_then_starts_the_session() {
        let mut login = session();
        let _ = login.apply(logged_in("a@b.c", "r1"), AppState::Login);
        let step = login.intent(LoginIntent::JoinWorld("alpha".into()), AppState::Worlds);
        assert!(matches!(step.calls.as_slice(), [LoginCall::Join { world, .. }] if world == "alpha"));

        let step = login.apply(
            LoginReply::Joined(Ok(joined(ticket(Some("127.0.0.1:6466"), Some("https://127.0.0.1:6467/"))))),
            AppState::Worlds,
        );
        assert_eq!(step.store, StoreOp::Save("rotated".into()));
        assert_eq!(login.refresh.as_deref(), Some("rotated"));
        assert_eq!(step.goto, Some(AppState::Starting));
        let join = step.join.expect("a pending join");
        assert_eq!(join.ticket.jwt, "header.claims.sig");
        #[cfg(not(target_arch = "wasm32"))]
        assert_eq!(join.addr, "127.0.0.1:6466".parse().unwrap());
        assert!(login.playing_online);
        assert_eq!(LoginSession::home(Some(&login)), AppState::Worlds, "an online session ends on the list");
        assert_eq!(LoginSession::home(None), AppState::MainMenu);
    }

    #[test]
    fn a_ticket_without_this_platforms_address_is_an_error_line() {
        let mut login = session();
        let _ = login.apply(logged_in("a@b.c", "r1"), AppState::Login);
        let step = login.apply(LoginReply::Joined(Ok(joined(ticket(None, None)))), AppState::Worlds);
        assert_eq!(step.goto, None);
        assert!(step.join.is_none());
        assert_eq!(step.store, StoreOp::Save("rotated".into()), "the rotated token is kept anyway");
        assert_eq!(login.error, NO_ADDRESS_ERROR);
        assert!(!login.playing_online);
    }

    #[test]
    fn a_revoked_join_signs_out() {
        let mut login = session();
        let _ = login.apply(logged_in("a@b.c", "r1"), AppState::Login);
        let step = login.apply(LoginReply::Joined(Err(api(ApiErrorCode::Revoked))), AppState::Worlds);
        assert_eq!(step.goto, Some(AppState::Login));
        assert_eq!(step.store, StoreOp::Clear);
        assert!(!login.logged_in());
    }

    #[test]
    fn logout_forgets_the_login_and_tells_the_service() {
        let mut login = session();
        let _ = login.apply(logged_in("a@b.c", "r1"), AppState::Login);
        login.worlds = vec![WorldEntry { id: "w".into(), name: "W".into(), status: WorldStatus::Open }];
        let step = login.intent(LoginIntent::Logout, AppState::Worlds);
        assert_eq!(step.store, StoreOp::Clear);
        assert_eq!(step.goto, Some(LoginSession::logged_out_state()));
        assert!(matches!(step.calls.as_slice(), [LoginCall::Logout { token }] if token == "r1"));
        assert!(!awaited(&step.calls[0]), "nothing waits for a logout");
        assert!(!login.logged_in() && login.refresh.is_none() && login.worlds.is_empty());
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn quic_addresses_resolve() {
        assert_eq!(resolve_quic(Some("127.0.0.1:6466")), Ok("127.0.0.1:6466".parse().unwrap()));
        assert_eq!(resolve_quic(None), Err(NO_ADDRESS_ERROR.into()));
        assert_eq!(resolve_quic(Some(" ")), Err(NO_ADDRESS_ERROR.into()));
        assert!(resolve_quic(Some("no port")).is_err());
    }
}
