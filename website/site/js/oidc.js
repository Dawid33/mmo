// OpenID Connect authorization code + PKCE against Keycloak, for a static site.
// No globals: everything comes in through `env`
//   { config, pending: Storage, session: Storage, crypto: Crypto, fetch, now: () => ms }
// (both `pending` and `session` are localStorage in the browser).
//
// Each login in flight is kept as `orion.pending.<state>`, shared by every tab: a sign-up's
// mailed verification link finishes the login in a new tab, which must find it there.

export const SESSION_MAX_AGE_MS = 8 * 60 * 60 * 1000;
export const PENDING_MAX_AGE_MS = 60 * 60 * 1000;

const PENDING_PREFIX = "orion.pending.";
const SESSION_KEY = "orion.session";
const SCOPE = "openid profile email";

export class OidcError extends Error {
  constructor(code, message) {
    super(message ?? code);
    this.name = "OidcError";
    this.code = code;
  }
}

function base64url(bytes) {
  let bin = "";
  for (const b of bytes) bin += String.fromCharCode(b);
  return btoa(bin).replaceAll("+", "-").replaceAll("/", "_").replaceAll("=", "");
}

function random(crypto) {
  return base64url(crypto.getRandomValues(new Uint8Array(32)));
}

export async function challengeFor(verifier, crypto) {
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(verifier));
  return base64url(new Uint8Array(digest));
}

export function safeReturnTo(path) {
  return path === "/profile.html" ? "/profile.html" : "/";
}

const redirectUri = (env) => `${env.config.siteUrl}/callback.html`;
const endpoint = (env, name) => `${env.config.issuer}/protocol/openid-connect/${name}`;

// A pending login can be finished for PENDING_MAX_AGE_MS after it began.
const fresh = (env, pending) =>
  typeof pending?.at === "number" && env.now() - pending.at <= PENDING_MAX_AGE_MS;

// Drops pending logins that are too old to finish (or unreadable): abandoned logins.
function prunePending(env) {
  const keys = [];
  for (let i = 0; i < env.pending.length; i++) {
    const key = env.pending.key(i);
    if (key?.startsWith(PENDING_PREFIX)) keys.push(key);
  }
  for (const key of keys) if (!fresh(env, readJson(env.pending, key))) env.pending.removeItem(key);
}

export async function beginLogin(env, { prompt, kcAction, returnTo } = {}) {
  prunePending(env);
  const state = random(env.crypto);
  const nonce = random(env.crypto);
  const verifier = random(env.crypto);
  const params = new URLSearchParams({
    response_type: "code",
    client_id: env.config.clientId,
    scope: SCOPE,
    redirect_uri: redirectUri(env),
    state,
    nonce,
    code_challenge: await challengeFor(verifier, env.crypto),
    code_challenge_method: "S256",
  });
  if (prompt) params.set("prompt", prompt);
  if (kcAction) params.set("kc_action", kcAction);
  env.pending.setItem(PENDING_PREFIX + state, JSON.stringify({
    nonce, verifier, returnTo: safeReturnTo(returnTo), at: env.now(),
  }));
  return `${endpoint(env, "auth")}?${params}`;
}

function readJson(storage, key) {
  try {
    const v = JSON.parse(storage.getItem(key));
    return v && typeof v === "object" ? v : null;
  } catch {
    return null;
  }
}

function idTokenClaims(idToken) {
  try {
    const bytes = Uint8Array.from(atob(idToken.split(".")[1].replaceAll("-", "+").replaceAll("_", "/")),
      (c) => c.charCodeAt(0));
    const claims = JSON.parse(new TextDecoder().decode(bytes));
    if (claims && typeof claims === "object") return claims;
  } catch { /* fall through */ }
  throw new OidcError("bad_id_token", "the ID token is not readable");
}

export async function completeLogin(env, callbackUrl) {
  const query = new URL(callbackUrl).searchParams;
  const state = query.get("state");
  if (!state) throw new OidcError("no_pending_login");
  const pending = readJson(env.pending, PENDING_PREFIX + state);
  env.pending.removeItem(PENDING_PREFIX + state); // one try per login, whatever follows
  if (!fresh(env, pending)) throw new OidcError("no_pending_login");

  if (query.has("iss") && query.get("iss") !== env.config.issuer) throw new OidcError("issuer_mismatch");
  if (query.has("error")) throw new OidcError("provider_error", query.get("error"));
  const code = query.get("code");
  if (!code) throw new OidcError("provider_error", "no code in the callback");

  let tokens;
  try {
    const res = await env.fetch(endpoint(env, "token"), {
      method: "POST",
      headers: { "Content-Type": "application/x-www-form-urlencoded" },
      body: new URLSearchParams({
        grant_type: "authorization_code",
        client_id: env.config.clientId,
        code,
        redirect_uri: redirectUri(env),
        code_verifier: pending.verifier,
      }).toString(),
    });
    if (!res.ok) throw new Error(`token endpoint answered ${res.status}`);
    tokens = await res.json();
    if (!tokens || typeof tokens.id_token !== "string") throw new Error("no id_token in the answer");
  } catch (e) {
    throw new OidcError("token_exchange_failed", e.message);
  }

  // No signature check: the token came straight from the token endpoint over TLS
  // (OpenID Connect Core 3.1.3.7). Access and refresh tokens are dropped.
  const claims = idTokenClaims(tokens.id_token);
  const aud = Array.isArray(claims.aud) ? claims.aud : [claims.aud];
  if (claims.iss !== env.config.issuer || !aud.includes(env.config.clientId) || claims.nonce !== pending.nonce)
    throw new OidcError("bad_id_token", "issuer, audience or nonce do not match");

  env.session.setItem(SESSION_KEY, JSON.stringify({
    username: claims.preferred_username ?? "",
    email: claims.email ?? "",
    emailVerified: claims.email_verified === true,
    idToken: tokens.id_token,
    at: env.now(),
  }));

  const name = query.get("kc_action");
  const status = query.get("kc_action_status");
  return {
    returnTo: safeReturnTo(pending.returnTo),
    action: name ? { name, status } : null,
  };
}

export function currentSession(env) {
  const s = readJson(env.session, SESSION_KEY);
  if (!s || typeof s.at !== "number" || env.now() - s.at > SESSION_MAX_AGE_MS) {
    env.session.removeItem(SESSION_KEY);
    return null;
  }
  return { username: s.username, email: s.email, emailVerified: s.emailVerified };
}

export function logoutUrl(env) {
  const s = readJson(env.session, SESSION_KEY);
  const params = new URLSearchParams({
    client_id: env.config.clientId,
    post_logout_redirect_uri: `${env.config.siteUrl}/`,
  });
  if (s?.idToken) params.set("id_token_hint", s.idToken);
  env.session.removeItem(SESSION_KEY);
  return `${endpoint(env, "logout")}?${params}`;
}
