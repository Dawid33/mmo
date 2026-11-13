// OpenID Connect authorization code + PKCE against Keycloak, for a static site.
// No globals: everything comes in through `env`
//   { config, pending: Storage, session: Storage, crypto: Crypto, fetch, now: () => ms }
// (`pending` = sessionStorage, `session` = localStorage in the browser).

export const SESSION_MAX_AGE_MS = 8 * 60 * 60 * 1000;

const PENDING_KEY = "orion.pending";
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

export async function beginLogin(env, { prompt, kcAction, returnTo } = {}) {
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
  env.pending.setItem(PENDING_KEY, JSON.stringify({
    state, nonce, verifier, returnTo: safeReturnTo(returnTo),
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
  const pending = readJson(env.pending, PENDING_KEY);
  env.pending.removeItem(PENDING_KEY); // one try per login, whatever follows
  if (!pending) throw new OidcError("no_pending_login");

  const query = new URL(callbackUrl).searchParams;
  if (query.get("state") !== pending.state) throw new OidcError("state_mismatch");
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
