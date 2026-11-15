#!/usr/bin/env bash
# `just site-theme-check`: does Keycloak serve every page a player can reach in the `orion`
# login theme? Against realm `players`, client `site`, it fetches each page below with a
# cookie jar and checks the body for
#   - `data-orion-theme` (our template.ftl, not the base one),
#   - a `<link … css/site.css>` whose URL answers 200,
#   - `href="$SITE/"` (the brand link, from ORION_SITE_URL in the Keycloak container),
#   - no "Keycloak" (any case) in the visible text (tags, script and style bodies stripped:
#     resource URLs legitimately hold `/keycloak/`).
# Pages: login, register (prompt=create), reset password, error (a foreign redirect_uri:
# HTTP 400), device code, update password (logs in as `dev` first).
#
# Env: KC (http://127.0.0.1:8181), SITE (http://127.0.0.1:8182: redirect URI and brand link),
#      SKIP_LOGIN=1 skips the update-password page (a server without the `dev` user).
# Prints `OK <page>` / `FAIL <page>: <why>` per page; exit 0 only if every page is OK.
set -uo pipefail

KC="${KC:-http://127.0.0.1:8181}"
SITE="${SITE:-http://127.0.0.1:8182}"
SKIP_LOGIN="${SKIP_LOGIN:-0}"
REALM="$KC/realms/players"
# A fixed PKCE challenge: the flow never reaches the token step, so its verifier is unused.
CHALLENGE="E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
auth_url() { # <redirect uri>
    echo "$REALM/protocol/openid-connect/auth?client_id=site&response_type=code&scope=openid&redirect_uri=$1&code_challenge=$CHALLENGE&code_challenge_method=S256"
}
LOGIN_URL="$(auth_url "$SITE/callback.html")"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
jar="$work/cookies"
failures=0

fail() { # <page> <why>
    echo "FAIL $1: $2"
    failures=$((failures + 1))
}

# fetch <page> <url> [curl args…]: body to $work/<page>.html, prints the HTTP status.
fetch() {
    local page="$1" url="$2"
    shift 2
    curl -sS -b "$jar" -c "$jar" -o "$work/$page.html" -w '%{http_code}' "$@" "$url" 2>"$work/$page.err" ||
        echo "curl-error"
}

# check <page> <status> <expected status> [required substring]
check() {
    local page="$1" status="$2" want="$3" needle="${4:-}"
    local body="$work/$page.html"
    if [[ "$status" != "$want" ]]; then
        fail "$page" "HTTP $status, expected $want $(head -c 200 "$work/$page.err" 2>/dev/null)"
        return
    fi
    if ! grep -q 'data-orion-theme' "$body"; then
        fail "$page" "no data-orion-theme (not the orion template)"
        return
    fi
    local css
    css="$(grep -oE '<link[^>]*href="[^"]*css/site\.css"' "$body" | head -n1 | sed -E 's/.*href="([^"]*)"/\1/')"
    if [[ -z "$css" ]]; then
        fail "$page" "no <link> to css/site.css"
        return
    fi
    [[ "$css" == http* ]] || css="$KC$css"
    local css_status
    css_status="$(curl -sS -o /dev/null -w '%{http_code}' "$css" 2>/dev/null || echo "curl-error")"
    if [[ "$css_status" != "200" ]]; then
        fail "$page" "stylesheet $css answered $css_status"
        return
    fi
    if ! grep -qF "href=\"$SITE/\"" "$body"; then
        fail "$page" "no brand link href=\"$SITE/\""
        return
    fi
    if [[ -n "$needle" ]] && ! grep -qF "$needle" "$body"; then
        fail "$page" "missing $needle"
        return
    fi
    local visible
    visible="$(perl -0777 -pe 's/<script\b.*?<\/script>//gis; s/<style\b.*?<\/style>//gis; s/<!--.*?-->//gs; s/<[^>]*>//gs' "$body" |
        grep -io 'keycloak' | head -n1)"
    if [[ -n "$visible" ]]; then
        fail "$page" "visible text names Keycloak"
        return
    fi
    echo "OK $page"
}

status="$(fetch login "$LOGIN_URL")"
check login "$status" 200

status="$(fetch register "$LOGIN_URL&prompt=create")"
check register "$status" 200 'id="kc-register-form"'

status="$(fetch reset-password "$REALM/login-actions/reset-credentials?client_id=site")"
check reset-password "$status" 200

status="$(fetch error "$(auth_url "$SITE/evil.html")")"
check error "$status" 400

status="$(fetch device "$REALM/device")"
check device "$status" 200

if [[ "$SKIP_LOGIN" == "1" ]]; then
    echo "SKIP update-password (SKIP_LOGIN=1)"
else
    # A fresh login page in this jar, then post its form as `dev`. The answer is a redirect to
    # the site's callback, which need not be running: do not follow it.
    status="$(fetch login-form "$LOGIN_URL")"
    action="$(grep -oE 'id="kc-form-login"[^>]*action="[^"]*"' "$work/login-form.html" |
        sed -E 's/.*action="([^"]*)"/\1/; s/&amp;/\&/g')"
    if [[ "$status" != "200" || -z "$action" ]]; then
        fail update-password "no login form to post (HTTP $status)"
    else
        status="$(fetch login-post "$action" --data-urlencode username=dev --data-urlencode password=dev-password)"
        if [[ "$status" != "302" ]]; then
            fail update-password "logging in as dev answered HTTP $status, expected 302"
        else
            status="$(fetch update-password "$LOGIN_URL&kc_action=UPDATE_PASSWORD" -L --max-redirs 5)"
            check update-password "$status" 200 'name="password-new"'
        fi
    fi
fi

if ((failures > 0)); then
    echo "site-theme-check: $failures page(s) failed"
    exit 1
fi
echo "site-theme-check: all pages OK"
