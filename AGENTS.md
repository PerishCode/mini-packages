# Mini Packages AGENTS Guide

## Product Boundary

This project is a private-first npm registry for scoped packages. Keep the core small:

- free bearer tokens
- npm/pnpm publish compatibility
- npm/pnpm install compatibility
- Postgres-backed token and package metadata
- S3-compatible tarball storage

Do not grow the service into a public npm registry clone. In particular, avoid upstream proxying, search, web management UI, org/user systems, audit databases, package discovery, billing, or generalized RBAC unless explicitly requested.

## Repository Structure

```text
mini-packages/
├── api/                  # Rust API service
├── web/                  # SvelteKit shell, no product UI yet
├── e2e/                  # npm/pnpm smoke checks
├── docker-compose.yml    # local Postgres + MinIO + API + web
├── .env.example          # local runtime defaults
└── AGENTS.md             # project execution conventions
```

## Execution Conventions

- Keep the API shape close to `service.auth`: `handler/`, `service/`, `repo/`, `state/`, `config/`, and `telemetry/`.
- Keep `.task/` as local long-running task memory only. Do not commit it unless explicitly requested.
- Prefer exact scoped package behavior over broad npm registry compatibility.
- Treat package versions as immutable once ready.
- Keep token management in the HTTP API, not in S3 metadata.

## Local Stack

Expected local stack:

- `db`: Postgres
- `minio`: S3-compatible object storage
- `minio-init`: bucket bootstrap
- `api`: Rust registry service
- `web`: SvelteKit shell and future same-origin facade

## Minimal Verification

- `cd api && cargo test --locked`
- `cd web && pnpm build`
- `cd e2e && pnpm test`

