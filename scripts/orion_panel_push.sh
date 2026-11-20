#!/usr/bin/env bash
# Releases the world panel on orion (`just orion-panel-push`, after
# `just orion-panel-build`): builds the image (deploy/images/panel/Dockerfile, from a
# staging directory holding only the binary and the Dockerfile), pushes it to ECR as
# <repo>:latest — the repository's only tag — and sets it on the Deployment BY DIGEST
# (`kubectl set image … panel=<repo>@sha256:…`), then waits for the rollout. As
# scripts/orion_login_push.sh, except: one replica and Recreate, so the panel is away
# for the few seconds of the swap, and a start or stop running in the old pod is
# cut off (the world is left where that step got to: run it again from the new pod).
# Before the panel root is applied there is no Deployment: the script only pushes.
# Env: KUBE_CONTEXT (orion), AWS_REGION (eu-central-1), PANEL_BIN
# (target/release/panel), IMAGE_REPOSITORY (default: the registry root's
# repository_urls["mmo/panel"], read with $TOFU, default tofu).
# The deploy orchestrator sets PUSH_ONLY=1 (build and push, no `set image`) and
# DIGEST_OUT=<file> (write `panel=<digest ref>` there instead of parsing stdout).
# The image is labelled with mmo.content-sha256 (the staged files' hash), which
# lets `just deploy --game` skip a release whose content did not change.
# The ECR password goes through a pipe (`--password-stdin`), never argv.
set -euo pipefail
cd "$(dirname "$0")/.."
ctx="${KUBE_CONTEXT:-orion}"
region="${AWS_REGION:-eu-central-1}"
bin="${PANEL_BIN:-target/release/panel}"
tofu="${TOFU:-tofu}"

[ -x "$bin" ] || { echo "orion-panel-push: no $bin (run just orion-panel-build)" >&2; exit 1; }
PYTHONPATH="$PWD/scripts" python3 -m deploy.images verify-dials panel

repo="${IMAGE_REPOSITORY:-}"
if [ -z "$repo" ]; then
    # tofu's stderr stays visible (expired credentials, an un-initialised root, …).
    if ! urls=$("$tofu" -chdir=deploy/envs/orion/registry output -json repository_urls) || [ -z "$urls" ]; then
        echo "orion-panel-push: could not read the registry root's repository_urls (just orion-init registry && just orion-apply registry)" >&2
        exit 1
    fi
    repo=$(printf '%s' "$urls" | python3 -I -c 'import json,sys; print(json.load(sys.stdin).get("mmo/panel",""))')
fi
if [ -z "$repo" ]; then
    echo "orion-panel-push: the registry root has no mmo/panel repository (plan and apply the registry root)" >&2
    exit 1
fi
registry="${repo%%/*}"
image="$repo:latest"

rev=$(git rev-parse --short HEAD)
if [ -n "$(git status --porcelain)" ]; then
    rev="$rev-dirty"
    echo "orion-panel-push: uncommitted checkout changes: revision $rev" >&2
fi

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
cp "$bin" "$stage/panel"
cp target/release/panel.build-dials "$stage/build-dials"
cp deploy/images/panel/Dockerfile "$stage/Dockerfile"
content=$(PYTHONPATH="$PWD/scripts" python3 -m deploy.images hash "$stage")
echo "orion-panel: building $image (revision $rev, content $content)"
docker build -q --label "org.opencontainers.image.revision=$rev" \
    --label "mmo.content-sha256=$content" -t "$image" "$stage" > /dev/null

aws ecr get-login-password --region "$region" | docker login --username AWS --password-stdin "$registry" > /dev/null
docker push -q "$image" > /dev/null
# The pushed image's digest in THIS repository (RepoDigests may list others).
digest_ref=$(docker inspect --format '{{range .RepoDigests}}{{println .}}{{end}}' "$image" |
    awk -v p="$repo@sha256:" 'index($0, p) == 1 { print; exit }' || true)
if [ -z "$digest_ref" ]; then
    echo "orion-panel-push: pushed $image but found no digest for $repo in its RepoDigests; nothing rolled out" >&2
    exit 1
fi
echo "orion-panel: pushed $image ($digest_ref, revision $rev)"

if [ -n "${DIGEST_OUT:-}" ]; then
    printf 'panel=%s\n' "$digest_ref" > "$DIGEST_OUT"
fi
if [ "${PUSH_ONLY:-0}" = 1 ]; then
    echo "orion-panel: pushed only; nothing rolled out"
    exit 0
fi

if current=$(kubectl --context "$ctx" -n panel get deploy/panel \
        -o jsonpath='{.spec.template.spec.containers[?(@.name=="panel")].image}' 2>&1); then
    if [ "$current" = "$digest_ref" ]; then
        echo "orion-panel: already running $digest_ref; nothing to roll out"
        exit 0
    fi
    echo "orion-panel: set image $digest_ref (revision $rev), was $current"
    if ! err=$(kubectl --context "$ctx" -n panel set image deploy/panel "panel=$digest_ref" 2>&1); then
        echo "$err" >&2
        echo "orion-panel-push: $image is pushed but the Deployment was NOT changed (kubectl set image failed); still running $current" >&2
        exit 2
    fi
    if ! kubectl --context "$ctx" -n panel rollout status deploy/panel --timeout=3m; then
        {
            echo "orion-panel-push: the new pod ($digest_ref, revision $rev) did not become ready (Recreate: no panel runs now)."
            echo "Way back (to the previous digest; works while that image is still in ECR, about a day, or in the node's cache):"
            echo "  kubectl --context $ctx -n panel rollout undo deploy/panel"
        } >&2
        exit 1
    fi
    echo "orion-panel: released $digest_ref (revision $rev)"
elif [[ "$current" == *NotFound* ]]; then
    echo "image pushed: $image ($digest_ref); apply the panel root"
else
    echo "$current" >&2
    echo "orion-panel-push: $image is pushed but NOT rolled out — fix the cluster access and run \`kubectl --context $ctx -n panel set image deploy/panel panel=$digest_ref\`" >&2
    exit 2
fi
