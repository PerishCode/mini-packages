# Mini Packages

Mini Packages is a private-first npm registry for personal and small-team scoped packages.

The core boundary is intentionally small:

- bearer token auth
- HTTP token management
- npm/pnpm publish compatibility
- npm/pnpm install compatibility
- scoped packages only
- Postgres metadata
- S3-compatible tarball storage

It does not proxy npmjs.org. Consumers must map private scopes in `.npmrc`:

```ini
@your-scope:registry=http://localhost:3333/
//localhost:3333/:_authToken=mpr_xxx
```

## Local Development

```sh
cp .env.example .env
docker compose up -d db minio minio-init api
```

Create a real admin token from the bootstrap token:

```sh
curl -s http://localhost:3333/api/v1/tokens \
  -H 'Authorization: Bearer dev-bootstrap-admin-token' \
  -H 'Content-Type: application/json' \
  -d '{"name":"local-admin","admin":true,"claims":{"read":["@demo/*"],"publish":["@demo/*"]}}'
```

## Scope

Supported:

- `@scope/name` packages only
- npm and pnpm publish/install
- bearer tokens through `_authToken`
- token create/list/get/rotate/revoke/claims
- dist-tag list/add/remove with safe label validation

Out of scope for the core service:

- upstream registry proxy/cache
- search
- npm login/basic auth
- web management UI
- public registry features
- audit database

