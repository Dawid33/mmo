#!/usr/bin/env bash
# Elastic 6's destructive twin-only rehearsal: move login from its old
# StatefulSet to the pg-management CNPG cluster. Never point this at orion.
set -euo pipefail
cd "$(dirname "$0")/.."

ctx="${TWIN_CONTEXT:-k3d-mmo}"
[ "$ctx" = "k3d-mmo" ] || { echo "twin-pg-move-login: refusing non-twin context '$ctx'" >&2; exit 2; }
ns=login
root=deploy/envs/local/login
dump="target/twin/login-before-management.dump"
plan="$(pwd)/target/twin/login-management.tfplan"
mkdir -p target/twin
rm -f "$dump"
trap 'rm -f "$dump"' EXIT

k() { kubectl --context "$ctx" -n "$ns" "$@"; }
km() { kubectl --context "$ctx" -n pg-management "$@"; }

echo "twin-pg-move-login: checking the management databases"
km wait database/login database/keycloak --for=jsonpath='{.status.applied}'=true --timeout=300s

echo "twin-pg-move-login: checking the world and recording a live join"
tok=$(k exec deploy/login -- login account issue-refresh twin-pg-move@example.com 2>/dev/null |
    sed '/^[[:space:]]*$/d' | tail -1)
[ -n "$tok" ] || { echo "twin-pg-move-login: login issued no token" >&2; exit 1; }
join() {
    local response code body rotated
    response=$(jq -nc --arg t "$tok" '{refresh_token: $t, world: "twin"}' |
        curl -sS --max-time 30 -w '\n%{http_code}' -H 'Host: game.localhost' \
            -H 'Content-Type: application/json' --data-binary @- \
            http://127.0.0.1:8480/login/v1/join)
    code=${response##*$'\n'}
    body=${response%$'\n'*}
    [ "$code" = 200 ] || return 1
    rotated=$(printf '%s' "$body" | jq -r '.refresh_token // empty')
    [ -n "$rotated" ] || return 1
    tok=$rotated
}
join
auth_before=$(kubectl --context "$ctx" -n world-twin get pods -l app.kubernetes.io/component=host \
    -o jsonpath='{.items[*].metadata.uid}')

echo "twin-pg-move-login: stopping login writers"
k scale deploy/login --replicas=0
k wait pod -l app.kubernetes.io/name=login --for=delete --timeout=120s
before=$(k exec login-postgres-0 -- psql -U login -d login -Atc 'select count(*) from accounts')

echo "twin-pg-move-login: dumping the old login database"
k exec login-postgres-0 -- pg_dump -Fc -U login -d login > "$dump"
[ -s "$dump" ] || { echo "twin-pg-move-login: pg_dump wrote an empty file" >&2; exit 1; }

primary=$(km get pods -l 'cnpg.io/cluster=pg-management,cnpg.io/instanceRole=primary' \
    -o jsonpath='{.items[0].metadata.name}')
[ -n "$primary" ] || { echo "twin-pg-move-login: no pg-management primary" >&2; exit 1; }
echo "twin-pg-move-login: restoring into pg-management as role login"
km exec -i "$primary" -c postgres -- pg_restore -U postgres --no-owner --role=login -d login < "$dump"
owners=$(km exec "$primary" -c postgres -- psql -U postgres -d login -Atc \
    "select tableowner, count(*) from pg_tables where schemaname = 'public' group by 1")
case "$owners" in login\|*) ;; *) echo "twin-pg-move-login: unexpected owners: $owners" >&2; exit 1 ;; esac
after=$(km exec "$primary" -c postgres -- psql -U postgres -d login -Atc 'select count(*) from accounts')
[ "$after" = "$before" ] || { echo "twin-pg-move-login: account count $before -> $after" >&2; exit 1; }

echo "twin-pg-move-login: switching the login root to pg-management and stopping the old StatefulSet"
TF_DATA_DIR="$(pwd)/target/twin/login/.terraform" TF_PLUGIN_CACHE_DIR="$(pwd)/target/tofu-plugin-cache" \
    tofu -chdir="$root" plan -input=false -out="$plan" \
    -var=use_management_db=true -var=own_postgres=stopped
TF_DATA_DIR="$(pwd)/target/twin/login/.terraform" TF_PLUGIN_CACHE_DIR="$(pwd)/target/tofu-plugin-cache" \
    tofu -chdir="$root" apply -input=false "$plan"
k rollout status deploy/login --timeout=300s
k wait statefulset/login-postgres --for=jsonpath='{.spec.replicas}'=0 --timeout=120s

echo "twin-pg-move-login: rejoining through the moved login store"
join
issued=$(k exec deploy/login -- login account issue-refresh twin-pg-move-after@example.com 2>/dev/null |
    sed '/^[[:space:]]*$/d' | tail -1)
[ -n "$issued" ] || { echo "twin-pg-move-login: login could not issue a refresh token" >&2; exit 1; }
auth_after=$(kubectl --context "$ctx" -n world-twin get pods -l app.kubernetes.io/component=host \
    -o jsonpath='{.items[*].metadata.uid}')
[ "$auth_after" = "$auth_before" ] || {
    echo "twin-pg-move-login: host pods changed during the move: $auth_before -> $auth_after" >&2
    exit 1
}
connections=$(km exec "$primary" -c postgres -- psql -U postgres -d login -Atc \
    'select count(*) from pg_stat_activity')
[ "$connections" -lt 100 ] || { echo "twin-pg-move-login: $connections connections >= 100" >&2; exit 1; }
echo "twin-pg-move-login: PASS (accounts $before -> $after, $connections/100 connections)"
