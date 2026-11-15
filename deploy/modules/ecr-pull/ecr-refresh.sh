#!/bin/sh
# Rewrites the pull secret $PULL_SECRET (kubernetes.io/dockerconfigjson) in this
# pod's namespace with a fresh ECR token (CronJob ecr-refresh, deploy/modules/login/ecr.tf).
# POSIX sh (the refresher image's busybox). Env: AWS_ACCESS_KEY_ID /
# AWS_SECRET_ACCESS_KEY (the pull-only user), AWS_DEFAULT_REGION, REGISTRY_HOST,
# PULL_SECRET, TMPDIR (an in-memory emptyDir).
# The token never reaches a process argument or a log line: it travels through
# files (mode 600, removed on exit) and pipes, and is spliced into the patch by
# the shell's builtin printf. On any failure the Secret is left as it was and the
# script exits non-zero.
set -eu
umask 077
: "${AWS_DEFAULT_REGION:?}" "${REGISTRY_HOST:?}" "${PULL_SECRET:?}"

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
fail() { echo "ecr-refresh: $*" >&2; exit 1; }

aws ecr get-login-password --region "$AWS_DEFAULT_REGION" > "$work/raw" ||
    fail "aws ecr get-login-password failed; $PULL_SECRET unchanged"
tr -d '\r\n' < "$work/raw" > "$work/token"
[ -s "$work/token" ] || fail "empty token; $PULL_SECRET unchanged"
token=$(cat "$work/token")
case $token in
    *[!A-Za-z0-9+/=]*) fail "the token is not base64; $PULL_SECRET unchanged" ;;
esac

auth=$({ printf 'AWS:'; cat "$work/token"; } | base64 | tr -d '\n')
printf '{"auths":{"%s":{"username":"AWS","password":"%s","auth":"%s"}}}' \
    "$REGISTRY_HOST" "$token" "$auth" > "$work/config.json"
unset token auth
config=$(base64 < "$work/config.json" | tr -d '\n')
printf '{"data":{".dockerconfigjson":"%s"}}' "$config" > "$work/patch.json"
unset config

if ! kubectl patch secret "$PULL_SECRET" --type=merge --patch-file="$work/patch.json" \
        > /dev/null 2> "$work/err"; then
    # Show kubectl's message unless it quotes the token.
    if grep -qF -f "$work/token" "$work/err"; then
        fail "kubectl patch failed (its message held the token, not shown); $PULL_SECRET unchanged"
    fi
    cat "$work/err" >&2
    fail "kubectl patch failed; $PULL_SECRET unchanged"
fi
echo "$PULL_SECRET refreshed"
