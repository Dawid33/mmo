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
