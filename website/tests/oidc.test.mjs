import test from "node:test";
import assert from "node:assert/strict";
import {
  beginLogin, completeLogin, currentSession, logoutUrl, safeReturnTo,
  challengeFor, OidcError, SESSION_MAX_AGE_MS, PENDING_MAX_AGE_MS,
} from "../site/js/oidc.js";

const ISSUER = "https://kc.test/realms/players";
const SITE = "https://site.test";

// The Web Storage surface oidc.js may use (localStorage in the browser).
function storage() {
  const m = new Map();
  return {
    getItem: (k) => (m.has(k) ? m.get(k) : null),
    setItem: (k, v) => void m.set(k, String(v)),
    removeItem: (k) => void m.delete(k),
    key: (i) => [...m.keys()][i] ?? null,
    get length() { return m.size; },
    has: (k) => m.has(k),
    keys: () => [...m.keys()],
  };
}

const pendingKey = (state) => `orion.pending.${state}`;
const pendingKeys = (env) => env.pending.keys().filter((k) => k.startsWith("orion.pending."));

function fakeEnv({ fetch } = {}) {
  const calls = [];
  const env = {
    config: { issuer: ISSUER, clientId: "site", siteUrl: SITE },
    pending: storage(),
    session: storage(),
    crypto: globalThis.crypto,
    now: () => env._now,
    _now: 1_000_000,
    calls,
  };
  env.fetch = async (url, init) => {
    calls.push({ url, init });
    if (!fetch) throw new Error("unexpected fetch");
    return fetch(url, init);
  };
  return env;
}

function b64url(obj) {
  return Buffer.from(JSON.stringify(obj)).toString("base64url");
}
const idToken = (claims) => `x.${b64url(claims)}.y`;

// Begin a login and return its state plus the pending entry stored for it.
async function started(env, opts = {}) {
  const state = new URL(await beginLogin(env, opts)).searchParams.get("state");
  return { state, ...JSON.parse(env.pending.getItem(pendingKey(state))) };
}

const goodClaims = (p, over = {}) => ({
  iss: ISSUER, aud: "site", nonce: p.nonce,
  preferred_username: "dev", email: "dev@x.test", email_verified: true, ...over,
});
const tokenOk = (claims) => async () => ({
  ok: true, status: 200, json: async () => ({ id_token: idToken(claims), access_token: "a", refresh_token: "r" }),
});
const cb = (params) => `${SITE}/callback.html?${new URLSearchParams(params)}`;

test("pkce challenge matches RFC 7636 appendix B", async () => {
  assert.equal(await challengeFor("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk", crypto),
               "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
});

test("beginLogin builds the request and remembers it", async () => {
  const env = fakeEnv();
  const url = new URL(await beginLogin(env, { prompt: "create", returnTo: "/profile.html" }));
  assert.equal(url.origin + url.pathname, "https://kc.test/realms/players/protocol/openid-connect/auth");
  assert.equal(url.searchParams.get("response_type"), "code");
  assert.equal(url.searchParams.get("client_id"), "site");
  assert.equal(url.searchParams.get("redirect_uri"), "https://site.test/callback.html");
  assert.equal(url.searchParams.get("scope"), "openid profile email");
  assert.equal(url.searchParams.get("prompt"), "create");
  assert.equal(url.searchParams.get("code_challenge_method"), "S256");
  const state = url.searchParams.get("state");
  assert.deepEqual(pendingKeys(env), [pendingKey(state)], "one pending entry, keyed by its state");
  const p = JSON.parse(env.pending.getItem(pendingKey(state)));
  assert.equal(url.searchParams.get("nonce"), p.nonce);
  assert.equal(url.searchParams.get("code_challenge"), await challengeFor(p.verifier, crypto));
  assert.equal(p.returnTo, "/profile.html");
  assert.equal(p.at, env._now);
});

test("beginLogin passes kc_action and omits unset options", async () => {
  const env = fakeEnv();
  const url = new URL(await beginLogin(env, { kcAction: "UPDATE_PASSWORD" }));
  assert.equal(url.searchParams.get("kc_action"), "UPDATE_PASSWORD");
  assert.equal(url.searchParams.has("prompt"), false);
  const state = url.searchParams.get("state");
  assert.equal(JSON.parse(env.pending.getItem(pendingKey(state))).returnTo, "/");
});

test("safeReturnTo accepts only the two pages", () => {
  for (const bad of ["https://evil.test/", "//evil.test", "/profile.html?x=1", "javascript:alert(1)", undefined])
    assert.equal(safeReturnTo(bad), "/");
  assert.equal(safeReturnTo("/profile.html"), "/profile.html");
  assert.equal(safeReturnTo("/"), "/");
});

test("completeLogin stores the session and drops the pending login", async () => {
  const env = fakeEnv();
  const p = await started(env, { returnTo: "/profile.html" });
  const answer = tokenOk(goodClaims(p));
  env.fetch = async (url, init) => (env.calls.push({ url, init }), answer());
  const res = await completeLogin(env, cb({ code: "c0de", state: p.state, iss: ISSUER }));
  assert.deepEqual(res, { returnTo: "/profile.html", action: null });
  assert.deepEqual(pendingKeys(env), []);
  const s = JSON.parse(env.session.getItem("orion.session"));
  assert.equal(s.username, "dev");
  assert.equal(s.email, "dev@x.test");
  assert.equal(s.emailVerified, true);
  assert.ok(s.idToken);
  assert.equal(s.access_token, undefined);
  assert.equal(s.refresh_token, undefined);
  assert.deepEqual(currentSession(env), { username: "dev", email: "dev@x.test", emailVerified: true });
  const call = env.calls.at(-1);
  assert.equal(call.url, `${ISSUER}/protocol/openid-connect/token`);
  assert.equal(call.init.method, "POST");
  assert.equal(call.init.headers["Content-Type"], "application/x-www-form-urlencoded");
  const body = new URLSearchParams(call.init.body);
  assert.equal(body.get("grant_type"), "authorization_code");
  assert.equal(body.get("client_id"), "site");
  assert.equal(body.get("code"), "c0de");
  assert.equal(body.get("redirect_uri"), `${SITE}/callback.html`);
  assert.equal(body.get("code_verifier"), p.verifier);
});

test("completeLogin accepts a callback without an iss parameter", async () => {
  const env = fakeEnv({});
  const p = await started(env);
  env.fetch = async () => tokenOk(goodClaims(p))();
  const res = await completeLogin(env, cb({ code: "c", state: p.state }));
  assert.equal(res.returnTo, "/");
});

test("completeLogin without a pending login", async () => {
  const env = fakeEnv();
  await assert.rejects(completeLogin(env, cb({ code: "c", state: "s" })),
    (e) => e instanceof OidcError && e.code === "no_pending_login");
  assert.equal(env.session.has("orion.session"), false);
  assert.equal(env.calls.length, 0);
});

test("a callback whose state matches no pending login leaves the others alone", async () => {
  const env = fakeEnv();
  const p = await started(env);
  for (const params of [{ code: "c", state: p.state + "x", iss: ISSUER }, { code: "c", iss: ISSUER }]) {
    await assert.rejects(completeLogin(env, cb(params)),
      (e) => e instanceof OidcError && e.code === "no_pending_login");
  }
  assert.equal(env.calls.length, 0, "no fetch");
  assert.equal(env.session.has("orion.session"), false);
  assert.deepEqual(pendingKeys(env), [pendingKey(p.state)], "the real pending login is untouched");
});

test("two logins pending at once each complete with their own verifier", async () => {
  // A sign-up's mailed link finishes the login in another tab: every tab sees every pending login.
  const env = fakeEnv();
  const a = await started(env, { returnTo: "/profile.html" });
  const b = await started(env, { prompt: "create" });
  assert.notEqual(a.state, b.state);
  assert.deepEqual(pendingKeys(env).sort(), [pendingKey(a.state), pendingKey(b.state)].sort());
  for (const [p, returnTo] of [[b, "/"], [a, "/profile.html"]]) {
    env.fetch = async (url, init) => (env.calls.push({ url, init }), tokenOk(goodClaims(p))());
    const res = await completeLogin(env, cb({ code: `code-${p.state}`, state: p.state, iss: ISSUER }));
    assert.equal(res.returnTo, returnTo);
    const body = new URLSearchParams(env.calls.at(-1).init.body);
    assert.equal(body.get("code_verifier"), p.verifier);
    assert.equal(env.pending.has(pendingKey(p.state)), false, "its own entry is gone");
  }
  assert.deepEqual(pendingKeys(env), []);
});

test("an expired pending login is refused and removed", async () => {
  const env = fakeEnv();
  const p = await started(env);
  env._now += PENDING_MAX_AGE_MS + 1;
  await assert.rejects(completeLogin(env, cb({ code: "c", state: p.state, iss: ISSUER })),
    (e) => e instanceof OidcError && e.code === "no_pending_login");
  assert.equal(env.calls.length, 0, "no fetch");
  assert.equal(env.session.has("orion.session"), false);
  assert.deepEqual(pendingKeys(env), []);
});

test("a pending login is still good at exactly PENDING_MAX_AGE_MS", async () => {
  const env = fakeEnv();
  const p = await started(env);
  env._now += PENDING_MAX_AGE_MS;
  env.fetch = async () => tokenOk(goodClaims(p))();
  await completeLogin(env, cb({ code: "c", state: p.state }));
  assert.ok(currentSession(env));
});

test("beginLogin prunes expired and unreadable pending logins, nothing else", async () => {
  const env = fakeEnv();
  const old = await started(env);
  env._now += PENDING_MAX_AGE_MS / 2;
  const recent = await started(env);
  env.pending.setItem("orion.pending.broken", "{not json");
  env.pending.setItem("orion.session", "kept");
  env.pending.setItem("other.key", "kept");
  env._now += PENDING_MAX_AGE_MS / 2 + 1; // `old` is now over the limit, `recent` is not
  const fresh = await started(env);
  assert.deepEqual(pendingKeys(env).sort(), [pendingKey(recent.state), pendingKey(fresh.state)].sort());
  assert.equal(env.pending.has(pendingKey(old.state)), false);
  assert.equal(env.pending.getItem("orion.session"), "kept");
  assert.equal(env.pending.getItem("other.key"), "kept");
});

for (const [name, mutate, code] of [
  ["wrong iss parameter", (p) => ({ code: "c", state: p.state, iss: "https://evil.test/realms/players" }), "issuer_mismatch"],
  ["error=access_denied", (p) => ({ error: "access_denied", state: p.state, iss: ISSUER }), "provider_error"],
]) {
  test(`completeLogin rejects ${name}`, async () => {
    const env = fakeEnv();
    const p = await started(env);
    await assert.rejects(completeLogin(env, cb(mutate(p))),
      (e) => e instanceof OidcError && e.code === code);
    assert.equal(env.calls.length, 0, "no fetch");
    assert.equal(env.session.has("orion.session"), false);
    assert.deepEqual(pendingKeys(env), [], "pending removed even on failure");
  });
}

for (const [name, over] of [
  ["aud", { aud: "other" }],
  ["nonce", { nonce: "nope" }],
  ["iss", { iss: "https://evil.test/realms/players" }],
]) {
  test(`completeLogin rejects an id token with a wrong ${name}`, async () => {
    const env = fakeEnv();
    const p = await started(env);
    env.fetch = async () => tokenOk(goodClaims(p, over))();
    await assert.rejects(completeLogin(env, cb({ code: "c", state: p.state })),
      (e) => e instanceof OidcError && e.code === "bad_id_token");
    assert.equal(env.session.has("orion.session"), false);
    assert.deepEqual(pendingKeys(env), []);
  });
}

test("completeLogin accepts an id token whose aud is an array containing the client", async () => {
  const env = fakeEnv();
  const p = await started(env);
  env.fetch = async () => tokenOk(goodClaims(p, { aud: ["site", "other"] }))();
  await completeLogin(env, cb({ code: "c", state: p.state }));
  assert.ok(currentSession(env));
});

test("a failed token exchange is token_exchange_failed", async () => {
  for (const answer of [
    { ok: false, status: 400, json: async () => ({ error: "invalid_grant" }) },
    { ok: true, status: 200, json: async () => ({ access_token: "a" }) },
  ]) {
    const env = fakeEnv();
    const p = await started(env);
    env.fetch = async () => answer;
    await assert.rejects(completeLogin(env, cb({ code: "c", state: p.state })),
      (e) => e instanceof OidcError && e.code === "token_exchange_failed");
    assert.equal(env.session.has("orion.session"), false);
    assert.deepEqual(pendingKeys(env), []);
  }
});

test("a malformed id token is bad_id_token", async () => {
  const env = fakeEnv();
  const p = await started(env);
  env.fetch = async () => ({ ok: true, status: 200, json: async () => ({ id_token: "garbage" }) });
  await assert.rejects(completeLogin(env, cb({ code: "c", state: p.state })),
    (e) => e instanceof OidcError && e.code === "bad_id_token");
  assert.deepEqual(pendingKeys(env), []);
});

test("a cancelled password change keeps the session", async () => {
  const env = fakeEnv();
  const p = await started(env, { kcAction: "UPDATE_PASSWORD", returnTo: "/profile.html" });
  env.fetch = async () => tokenOk(goodClaims(p))();
  const res = await completeLogin(env, cb({
    code: "c", state: p.state, iss: ISSUER,
    kc_action: "UPDATE_PASSWORD", kc_action_status: "cancelled",
  }));
  assert.deepEqual(res.action, { name: "UPDATE_PASSWORD", status: "cancelled" });
  assert.equal(res.returnTo, "/profile.html");
  assert.ok(currentSession(env));
});

test("currentSession expires after SESSION_MAX_AGE_MS", async () => {
  const env = fakeEnv();
  const p = await started(env);
  env.fetch = async () => tokenOk(goodClaims(p))();
  await completeLogin(env, cb({ code: "c", state: p.state }));
  env._now += SESSION_MAX_AGE_MS;
  assert.ok(currentSession(env), "still valid at exactly the limit");
  env._now += 1;
  assert.equal(currentSession(env), null);
  assert.equal(env.session.has("orion.session"), false);
});

test("currentSession is null for absent or unparsable entries", () => {
  const env = fakeEnv();
  assert.equal(currentSession(env), null);
  env.session.setItem("orion.session", "{not json");
  assert.equal(currentSession(env), null);
});

test("logoutUrl carries the hint and clears the session", async () => {
  const env = fakeEnv();
  const p = await started(env);
  const claims = goodClaims(p);
  env.fetch = async () => tokenOk(claims)();
  await completeLogin(env, cb({ code: "c", state: p.state }));
  const url = new URL(logoutUrl(env));
  assert.equal(url.origin + url.pathname, `${ISSUER}/protocol/openid-connect/logout`);
  assert.equal(url.searchParams.get("client_id"), "site");
  assert.equal(url.searchParams.get("post_logout_redirect_uri"), `${SITE}/`);
  assert.equal(url.searchParams.get("id_token_hint"), idToken(claims));
  assert.equal(env.session.has("orion.session"), false);
});
