#!/usr/bin/env bash
# Releases the login service on orion (`just orion-login-push`, after
# `just orion-login-build`): builds the image (deploy/images/login/Dockerfile,
# from a staging directory holding only the binary and the Dockerfile), pushes
# it to ECR as <repo>:<git short sha>[-dirty], then sets it on the Deployment
# (`kubectl set image`, a rolling update: with two replicas logins continue) and
# waits for the rollout. A failed rollout leaves the old pods serving and prints
# the way back (`kubectl rollout undo`).
# Before the login root is applied there is no Deployment: the image is also
# pushed as <repo>:bootstrap (what the login module starts from) and the script
# says to apply the login root.
# Env: KUBE_CONTEXT (orion), AWS_REGION (eu-central-1), LOGIN_BIN
# (target/release/login), IMAGE_REPOSITORY (default: the registry root's
# `repository_url` output, read with $TOFU, default tofu).
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
    if ! repo=$("$tofu" -chdir=deploy/envs/orion/registry output -raw repository_url 2>/dev/null) || [ -z "$repo" ]; then
        echo "orion-login-push: no ECR repository: the registry root is not applied (or not initialised): just orion-init registry && just orion-apply registry" >&2
        exit 1
    fi
fi
registry="${repo%%/*}"

tag=$(git rev-parse --short HEAD)
if [ -n "$(git status --porcelain -- crates Cargo.lock)" ]; then
    tag="$tag-dirty"
    echo "orion-login-push: uncommitted changes under crates/ or Cargo.lock: tagging $tag" >&2
fi
image="$repo:$tag"

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
cp "$bin" "$stage/login"
cp deploy/images/login/Dockerfile "$stage/Dockerfile"
echo "orion-login: building $image"
docker build -q -t "$image" "$stage" > /dev/null

aws ecr get-login-password --region "$region" | docker login --username AWS --password-stdin "$registry" > /dev/null
docker push -q "$image" > /dev/null
echo "orion-login: pushed $image"

if out=$(kubectl --context "$ctx" -n login get deploy/login -o name 2>&1); then
    echo "orion-login: set image $image"
    if ! { kubectl --context "$ctx" -n login set image deploy/login "login=$image" &&
           kubectl --context "$ctx" -n login rollout status deploy/login --timeout=5m; }; then
        {
            echo "orion-login-push: the new pods did not become ready."
            echo "The old pods are still serving (a rolling update keeps them until the new ones are Ready)."
            echo "Way back:"
            echo "  kubectl --context $ctx -n login rollout undo deploy/login"
        } >&2
        exit 1
    fi
    echo "orion-login: released $image"
elif [[ "$out" == *NotFound* ]]; then
    # Absent Deployment (or namespace): the login root is not applied yet. The
    # module starts from :bootstrap.
    docker tag "$image" "$repo:bootstrap"
    docker push -q "$repo:bootstrap" > /dev/null
    echo "image pushed: $image (and :bootstrap); apply the login root"
else
    echo "$out" >&2
    echo "orion-login-push: $image is pushed but NOT rolled out — fix the cluster access and run \`kubectl --context $ctx -n login set image deploy/login login=$image\`" >&2
    exit 2
fi
