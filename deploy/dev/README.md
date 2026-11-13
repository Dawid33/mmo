# Dev Postgres

A local PostgreSQL 17 for the world store, run by `deploy/dev/compose.yml`.

- `just pg-up` starts it (waits until healthy) on `127.0.0.1:55432`.
- `just pg-down` stops it; the data stays in the named volume `mmo-dev-pg`.
- `just pg-reset` deletes the volume and starts again empty.
- `just pg-psql -c 'select version()'` runs `psql` inside the container.

Credentials are `mmo` / `mmo` for user, password and database. The dev URL is
`postgres://mmo:mmo@127.0.0.1:55432/mmo`. Tests create their own databases from
the maintenance database: `MMO_TEST_DATABASE_URL=postgres://mmo:mmo@127.0.0.1:55432/postgres`
(`just test-pg`).

This is for development only. The game box and the orion cluster do not use it.

# Dev Keycloak

The player identity provider, run by the same `deploy/dev/compose.yml` (service `keycloak`,
`quay.io/keycloak/keycloak:26.8.2` in `start-dev`).

- `just kc-up` starts it (waits until healthy) on `http://127.0.0.1:8180`; the admin
  console is at `/admin` with `admin` / `admin`.
- Like production, the realm verifies email. Keycloak's mail goes to the mail catcher
  `mailpit` (started with it): read it at `http://127.0.0.1:8025`.
- `just kc-down` stops and removes both containers. There is no data volume: every
  `kc-up` imports `keycloak/players-realm.json` afresh, so edit that file (not the admin
  console) to change the realm.
- `just site-theme-check` fetches every login page a player can reach and checks it is in
  the `orion` theme (`scripts/site_theme_check.sh`).

Realm `players` (issuer `http://127.0.0.1:8180/realms/players`): login theme `orion`,
registration on (username, email and password only), email verification required
(SMTP `mailpit:1025`), user `dev` / `dev-password`
(email `dev@localhost`, verified), and client `site`, the website: public, standard flow
with PKCE S256, redirect `http://127.0.0.1:8182/callback.html`, post-logout redirect
`http://127.0.0.1:8182/`.

Client `game`, the game: public, signs in by the device flow (the game shows a code, the player
enters it on Keycloak's page), PKCE S256 required (the device request carries a `code_challenge`,
the token request its `code_verifier`), redirects only to the browser build on
`http://127.0.0.1:1334` / `http://localhost:1334`; its access tokens carry the audience
`mmo-login` (mapper `login-service-audience`). It mirrors `deploy/modules/keycloak-realm/clients.tf`.
`just dev-login`, `dev-r2` and `dev-r3` start Keycloak and run the login service with
`VOXEL_OIDC_ISSUER=http://127.0.0.1:8180/realms/players`, so a player signs in as `dev` / `dev-password`.

The theme is mounted read-only from `website/theme/orion`. `start-dev` caches no themes,
so template, message and stylesheet edits show on reload; a `theme.properties` change
needs `just kc-down && just kc-up`. `ORION_SITE_URL` (`http://127.0.0.1:8182`) is the
brand link's target.
