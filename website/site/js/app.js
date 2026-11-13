// The site's pages: wires [data-action] buttons, shows the [data-show="in|out"] half that matches
// the session, fills [data-bind] fields, and finishes logins on callback.html. No inline script
// anywhere (the production CSP is script-src 'self'); every text written into the page goes in
// through textContent.
import { CONFIG } from "./config.js";
import { beginLogin, completeLogin, currentSession, logoutUrl, OidcError } from "./oidc.js";

// Pending logins live in localStorage, not per-tab sessionStorage: a sign-up's mailed
// verification link finishes the login in a new tab (oidc.js keys them by state).
const env = {
  config: CONFIG,
  pending: localStorage,
  session: localStorage,
  crypto: globalThis.crypto,
  fetch: (...a) => fetch(...a),
  now: () => Date.now(),
};

const NOTICE_KEY = "orion.notice";
const NOTICES = {
  "password-changed": { text: "Password changed.", className: "alert-success" },
  "password-not-changed": { text: "Password not changed.", className: null },
};

// no_pending_login is not an error to the player: see finishLogin.
const CALLBACK_ERRORS = {
  issuer_mismatch: "The answer came from the wrong login server, so it was ignored.",
  provider_error: "The login server did not finish the login.",
  token_exchange_failed: "The login server could not be reached to finish the login.",
  bad_id_token: "The login server's answer could not be checked, so it was ignored.",
};
const CALLBACK_UNKNOWN = "Something went wrong while finishing the login.";

const all = (selector) => document.querySelectorAll(selector);

async function go(makeUrl) {
  try {
    location.assign(await makeUrl());
  } catch (e) {
    console.error("orion: could not start the login", e);
  }
}

const ACTIONS = {
  login: () => go(() => beginLogin(env, { returnTo: location.pathname })),
  signup: () => go(() => beginLogin(env, { prompt: "create", returnTo: location.pathname })),
  "change-password": () =>
    go(() => beginLogin(env, { kcAction: "UPDATE_PASSWORD", returnTo: "/profile.html" })),
  logout: () => go(() => logoutUrl(env)),
};

function showSession(session) {
  const half = session ? "in" : "out";
  for (const el of all("[data-show]")) el.hidden = el.dataset.show !== half;
  if (!session) return;
  const values = {
    username: session.username,
    email: session.email,
    verified: session.emailVerified ? "verified" : "not verified",
  };
  for (const el of all("[data-bind]")) {
    const value = values[el.dataset.bind];
    if (value !== undefined) el.textContent = value;
  }
  for (const el of all('[data-bind="verified"]'))
    el.classList.toggle("badge-ok", session.emailVerified === true);
}

function showNotice() {
  const box = document.querySelector("[data-notice]");
  let key = null;
  try {
    key = sessionStorage.getItem(NOTICE_KEY);
    sessionStorage.removeItem(NOTICE_KEY); // shown once
  } catch { /* storage blocked: no notice */ }
  const notice = Object.hasOwn(NOTICES, key ?? "") ? NOTICES[key] : null;
  if (!box || !notice) return;
  box.querySelector("[data-notice-text]").textContent = notice.text;
  if (notice.className) box.classList.add(notice.className);
  box.hidden = false;
}

function noticeFor(action) {
  if (action?.name !== "UPDATE_PASSWORD") return null;
  return action.status === "success" ? "password-changed" : "password-not-changed";
}

function showCallback(title, part) {
  document.querySelector('[data-callback="title"]').textContent = title;
  document.querySelector('[data-callback="busy"]').hidden = true;
  document.querySelector(`[data-callback="${part}"]`).hidden = false;
}

async function finishLogin() {
  let result;
  try {
    result = await completeLogin(env, location.href);
  } catch (e) {
    history.replaceState(null, "", location.pathname);
    if (e instanceof OidcError && e.code === "no_pending_login") {
      // This login was already finished or given up elsewhere — typically the tab left on
      // Keycloak's "verify your email" page, after the mailed link logged in another tab.
      if (currentSession(env)) return location.replace("/");
      wireActions();
      showCallback("Almost there", "again"); // Keycloak's own session makes it one click
      return;
    }
    if (!(e instanceof OidcError)) console.error("orion: login failed", e);
    const text = e instanceof OidcError && Object.hasOwn(CALLBACK_ERRORS, e.code)
      ? CALLBACK_ERRORS[e.code]
      : CALLBACK_UNKNOWN;
    document.querySelector('[data-callback="message"]').textContent = text;
    showCallback("Login not finished", "failed");
    return;
  }
  // Logged in: nothing below may keep the player on this page.
  try {
    history.replaceState(null, "", location.pathname);
    const notice = noticeFor(result.action);
    if (notice) sessionStorage.setItem(NOTICE_KEY, notice);
  } catch (e) {
    console.error("orion: could not leave a notice", e);
  }
  location.replace(result.returnTo);
}

function wireActions() {
  for (const el of all("[data-action]")) {
    const action = ACTIONS[el.dataset.action];
    if (action) el.addEventListener("click", (event) => {
      event.preventDefault();
      action();
    });
  }
}

function main() {
  const page = document.body.dataset.page;
  if (page === "callback") return finishLogin();

  wireActions();
  for (const el of all('[data-link="play"]')) el.href = CONFIG.playUrl;
  for (const el of all('[data-link="chat"]')) el.href = CONFIG.chatUrl;

  showSession(currentSession(env));
  if (page === "profile") showNotice();
}

Promise.resolve()
  .then(main)
  .catch((e) => console.error("orion: page script failed", e));
