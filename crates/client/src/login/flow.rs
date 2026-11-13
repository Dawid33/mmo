//! Play online: logging in, listing the login service's worlds and joining one
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
//! Never log a password, a refresh token, a ticket or a JWT: nothing here
//! derives `Debug` over them.

#[cfg(not(target_arch = "wasm32"))]
use std::net::SocketAddr;

use bevy::prelude::*;
use crossbeam::channel::{unbounded, Receiver, Sender};
use login_api::{ApiErrorCode, JoinTicket, LoginResponse, WorldEntry};

use super::{store, LoginApi, LoginFailure};
use crate::session::{AppState, PendingJoin};

/// The notice after Register (the service mails a verification link;
/// unverified accounts may still log in).
pub const REGISTERED_NOTICE: &str = "Check your email to verify your account";
/// The notice after Forgot password (the same whether or not the email has an
/// account: no enumeration).
pub const RESET_NOTICE: &str = "If that email has an account, a reset link is on its way";
/// The world list's line when a ticket has no address this client can use.
pub const NO_ADDRESS_ERROR: &str = "That world cannot be reached from this client";

/// What a frontend button asked of the login service. Holds a password:
/// deliberately no `Debug`.
#[derive(Message, Clone)]
pub enum LoginIntent {
    /// The main menu's Play online.
    PlayOnline,
    Submit { email: String, password: String },
    Register { email: String, password: String },
    Forgot { email: String },
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
    Login { email: String, password: String },
    Register { email: String, password: String },
    /// `silent`: the refresh of a kept token (the browser's startup, or Play
    /// online with a kept token) rather than a typed login.
    Refresh { token: String, silent: bool },
    Worlds,
    Join { token: String, world: String },
    Logout { token: String },
    Reset { email: String },
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
    /// A login or a refresh (`silent`: of a kept token).
    Session { silent: bool, result: Result<LoginResponse, LoginFailure> },
    Registered(Result<(), LoginFailure>),
    Worlds(Result<Vec<WorldEntry>, LoginFailure>),
    Joined(Result<Joined, LoginFailure>),
    /// Logout is fire-and-forget: the local login is gone already.
    LoggedOut,
    ResetRequested(Result<(), LoginFailure>),
}

/// Whether `failure` says the refresh token itself is no good (revoked,
/// expired, unknown), as opposed to the service being down or failing.
fn refused_token(failure: &LoginFailure) -> bool {
    matches!(failure, LoginFailure::Api(e) if matches!(e.code, ApiErrorCode::Revoked | ApiErrorCode::BadCredentials))
}

/// Whether a call's reply counts toward [`LoginSession::busy`] (logout does
/// not: nothing waits for it).
fn awaited(call: &LoginCall) -> bool {
    !matches!(call, LoginCall::Logout { .. })
}

/// Runs one call against the service.
pub async fn perform(api: &LoginApi, call: LoginCall) -> LoginReply {
    match call {
        LoginCall::Login { email, password } => {
            LoginReply::Session { silent: false, result: api.login(&email, &password).await }
        }
        LoginCall::Register { email, password } => LoginReply::Registered(api.register(&email, &password).await),
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
        LoginCall::Reset { email } => LoginReply::ResetRequested(api.request_reset(&email).await),
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
}

impl Step {
    fn none() -> Self {
        Self { goto: None, store: StoreOp::Keep, calls: Vec::new(), join: None }
    }
    fn goto(state: AppState) -> Self {
        Self { goto: Some(state), ..Self::none() }
    }
    fn call(call: LoginCall) -> Self {
        Self { calls: vec![call], ..Self::none() }
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
    /// The signed-in account's email, once a login or refresh succeeded.
    pub account: Option<String>,
    pub worlds: Vec<WorldEntry>,
    /// The screens' error line ("" = none) and notice line.
    pub error: String,
    pub notice: String,
    /// The running world session was joined with a ticket: ending it returns
    /// to the world list.
    pub playing_online: bool,
    /// The password field is to be emptied (after a login).
    pub clear_password: bool,
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
            worlds: Vec::new(),
            error: String::new(),
            notice: String::new(),
            playing_online: false,
            clear_password: false,
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
    /// replies still record a token or an account, see [`Self::receive`]) and
    /// the screens are usable again.
    pub fn back(&mut self) {
        self.generation += 1;
        self.in_flight = 0;
    }

    /// A request is in flight: the screens' buttons are disabled.
    pub fn busy(&self) -> bool {
        self.in_flight > 0
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
        self.worlds.clear();
        self.playing_online = false;
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
                self.notice.clear();
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
            _ if busy => Step::none(),
            LoginIntent::Submit { email, password } => {
                self.error.clear();
                self.notice.clear();
                Step::call(LoginCall::Login { email: email.trim().into(), password })
            }
            LoginIntent::Register { email, password } => {
                self.error.clear();
                self.notice.clear();
                Step::call(LoginCall::Register { email: email.trim().into(), password })
            }
            LoginIntent::Forgot { email } => {
                self.error.clear();
                self.notice.clear();
                Step::call(LoginCall::Reset { email: email.trim().into() })
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
                self.notice.clear();
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
            let lines = (std::mem::take(&mut self.error), std::mem::take(&mut self.notice));
            let step = self.apply(reply, AppState::MainMenu);
            (self.error, self.notice) = lines;
            return step;
        }
        if !matches!(reply, LoginReply::LoggedOut) {
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
                self.error.clear();
                self.clear_password = true;
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
            LoginReply::Registered(result) | LoginReply::ResetRequested(result) if result.is_err() => {
                self.error = result.err().map(|f| f.to_string()).unwrap_or_default();
                Step::none()
            }
            LoginReply::Registered(_) => {
                self.notice = REGISTERED_NOTICE.into();
                Step::none()
            }
            LoginReply::ResetRequested(_) => {
                self.notice = RESET_NOTICE.into();
                Step::none()
            }
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
    if let Some(state) = step.goto {
        next.set(state);
    }
}

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

/// The service's answers, applied.
pub fn poll_login(
    login: Option<ResMut<LoginSession>>,
    state: Res<State<AppState>>,
    mut commands: Commands,
    mut next: ResMut<NextState<AppState>>,
) {
    let Some(mut login) = login else { return };
    let (replies, lost): (Vec<(Generation, LoginReply)>, bool) = match &login.worker {
        Some(worker) => {
            let replies = worker.replies.try_iter().collect();
            let lost = matches!(worker.replies.try_recv(), Err(crossbeam::channel::TryRecvError::Disconnected));
            (replies, lost)
        }
        None => return,
    };
    for (generation, reply) in replies {
        let step = login.receive(generation, reply, *state.get());
        perform_step(step, &mut login, &mut commands, &mut next);
    }
    if lost {
        error!("[login] the login-http worker stopped");
        login.worker_lost();
    }
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
        assert!(login.clear_password);
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
            (api(ApiErrorCode::BadCredentials), true),
            (api(ApiErrorCode::Internal), false),
            (api(ApiErrorCode::NotVerified), false),
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
    fn register_and_reset_show_their_notices() {
        let mut login = session();
        let _ = login.apply(LoginReply::Registered(Ok(())), AppState::Login);
        assert_eq!(login.notice, REGISTERED_NOTICE);
        let _ = login.apply(LoginReply::ResetRequested(Ok(())), AppState::Login);
        assert_eq!(login.notice, RESET_NOTICE);
        let short = LoginFailure::Api(ApiError { code: ApiErrorCode::BadRequest, message: "Password is too short".into() });
        let _ = login.apply(LoginReply::Registered(Err(short)), AppState::Login);
        assert_eq!(login.error, "Password is too short");
        let _ = login.apply(
            LoginReply::Session { silent: false, result: Err(api(ApiErrorCode::BadCredentials)) },
            AppState::Login,
        );
        assert_eq!(login.error, "Wrong email or password");
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
        let step = login.intent(LoginIntent::Submit { email: "a@b.c".into(), password: "p".into() }, AppState::Login);
        assert!(step.calls.is_empty());
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
