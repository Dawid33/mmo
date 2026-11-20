#!/usr/bin/env bash
# Releases the login service on orion (`just orion-login-push`, after
# `just orion-login-build`): builds the image (deploy/images/login/Dockerfile,
# from a staging directory holding only the binary and the Dockerfile), pushes
# it to ECR as <repo>:latest — the repository's only tag — and sets it on the
# Deployment BY DIGEST (`kubectl set image … login=<repo>@sha256:…`, a rolling
# update: with two replicas logins continue), then waits for the rollout. The
# digest is what makes a moving `latest` safe: every push has its own, so it
# always rolls out; the digest already running is "nothing to roll out".
# The build's git revision (short sha, `-dirty` if crates/ or Cargo.lock have
# uncommitted changes) is printed and kept as the image label
# org.opencontainers.image.revision.
# A failed rollout leaves the old pods serving and prints the way back
# (`kubectl rollout undo`, to the previous digest: it works while that image is
# still in ECR — untagged images expire about a day after their push — or in the
# node's cache); a failed `set image` changed nothing (exit 2).
# Before the login root is applied there is no Deployment: the script only
# pushes and says to apply the login root (the module starts from `latest`).
# Env: KUBE_CONTEXT (orion), AWS_REGION (eu-central-1), LOGIN_BIN
# (target/release/login), IMAGE_REPOSITORY (default: the registry root's
# `repository_url` output, read with $TOFU, default tofu).
# The deploy orchestrator sets PUSH_ONLY=1 (build and push, no `set image`) and
# DIGEST_OUT=<file> (write `login=<digest ref>` there instead of parsing stdout).
# The image is labelled with mmo.content-sha256 (the staged files' hash), which
# lets `just deploy --game` skip a release whose content did not change.
# The ECR password goes through a pipe (`--password-stdin`), never argv.
set -euo pipefail
cd "$(dirname "$0")/.."
ctx="${KUBE_CONTEXT:-orion}"
region="${AWS_REGION:-eu-central-1}"
bin="${LOGIN_BIN:-target/release/login}"
tofu="${TOFU:-tofu}"

[ -x "$bin" ] || { echo "orion-login-push: no $bin (run just orion-login-build)" >&2; exit 1; }

repo="${IMAGE_REPOSITORY:-}"
if [ -z "$repo" ]; then
    # tofu's stderr stays visible (expired credentials, an un-initialised root, …).
    if ! repo=$("$tofu" -chdir=deploy/envs/orion/registry output -raw repository_url) || [ -z "$repo" ]; then
        echo "orion-login-push: could not read the registry root's repository_url (is it initialised and applied? just orion-init registry && just orion-apply registry)" >&2
        exit 1
    fi
fi
registry="${repo%%/*}"
image="$repo:latest"

rev=$(git rev-parse --short HEAD)
if [ -n "$(git status --porcelain -- crates Cargo.lock)" ]; then
    rev="$rev-dirty"
    echo "orion-login-push: uncommitted changes under crates/ or Cargo.lock: revision $rev" >&2
fi

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
cp "$bin" "$stage/login"
cp deploy/images/login/Dockerfile "$stage/Dockerfile"
# One implementation of the content hash in scripts/deploy/images.py; the
# orchestrator computes the same hash from the same staged files.
content=$(PYTHONPATH="$PWD/scripts" python3 -m deploy.images hash "$stage")
echo "orion-login: building $image (revision $rev, content $content)"
docker build -q --label "org.opencontainers.image.revision=$rev" \
    --label "mmo.content-sha256=$content" -t "$image" "$stage" > /dev/null

aws ecr get-login-password --region "$region" | docker login --username AWS --password-stdin "$registry" > /dev/null
docker push -q "$image" > /dev/null
# The pushed image's digest in THIS repository (RepoDigests may list others).
digest_ref=$(docker inspect --format '{{range .RepoDigests}}{{println .}}{{end}}' "$image" |
    awk -v p="$repo@sha256:" 'index($0, p) == 1 { print; exit }' || true)
if [ -z "$digest_ref" ]; then
    echo "orion-login-push: pushed $image but found no digest for $repo in its RepoDigests; nothing rolled out" >&2
    exit 1
fi
echo "orion-login: pushed $image ($digest_ref, revision $rev)"

if [ -n "${DIGEST_OUT:-}" ]; then
    printf 'login=%s\n' "$digest_ref" > "$DIGEST_OUT"
fi
if [ "${PUSH_ONLY:-0}" = 1 ]; then
    echo "orion-login: pushed only; nothing rolled out"
    exit 0
fi

if current=$(kubectl --context "$ctx" -n login get deploy/login \
        -o jsonpath='{.spec.template.spec.containers[?(@.name=="login")].image}' 2>&1); then
    if [ "$current" = "$digest_ref" ]; then
        echo "orion-login: already running $digest_ref; nothing to roll out"
        exit 0
    fi
    echo "orion-login: set image $digest_ref (revision $rev), was $current"
    if ! err=$(kubectl --context "$ctx" -n login set image deploy/login "login=$digest_ref" 2>&1); then
        echo "$err" >&2
        echo "orion-login-push: $image is pushed but the Deployment was NOT changed (kubectl set image failed); still running $current" >&2
        exit 2
    fi
    if ! kubectl --context "$ctx" -n login rollout status deploy/login --timeout=5m; then
        {
            echo "orion-login-push: the new pods ($digest_ref, revision $rev) did not become ready."
            echo "The old pods are still serving (a rolling update keeps them until the new ones are Ready)."
            echo "Way back (to the previous digest; works while that image is still in ECR, about a day, or in the node's cache):"
            echo "  kubectl --context $ctx -n login rollout undo deploy/login"
        } >&2
        exit 1
    fi
    echo "orion-login: released $digest_ref (revision $rev)"
elif [[ "$current" == *NotFound* ]]; then
    # Absent Deployment (or namespace): the login root is not applied yet. The
    # module starts from the `latest` tag.
    echo "image pushed: $image ($digest_ref); apply the login root"
else
    echo "$current" >&2
    echo "orion-login-push: $image is pushed but NOT rolled out — fix the cluster access and run \`kubectl --context $ctx -n login set image deploy/login login=$digest_ref\`" >&2
    exit 2
fi
