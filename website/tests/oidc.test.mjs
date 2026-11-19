import test from "node:test";
import assert from "node:assert/strict";
import { beginLogin, completeLogin } from "../site/js/oidc.js";

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

// Regression: a sign-up's mailed verification link finished the login in another tab and both tabs ended in an
// error; observed in dev with verifyEmail on (82c1fd8f).
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
