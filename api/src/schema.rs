use sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseConnection, DbErr, Statement};

pub async fn apply(conn: &DatabaseConnection) -> Result<(), DbErr> {
    for sql in SCHEMA {
        conn.execute(Statement::from_string(
            DatabaseBackend::Postgres,
            sql.to_string(),
        ))
        .await?;
    }
    Ok(())
}

const SCHEMA: &[&str] = &[
    r#"
CREATE TABLE IF NOT EXISTS tokens (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    token_prefix TEXT NOT NULL,
    secret_hash TEXT NOT NULL,
    admin BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ NULL,
    rotated_at TIMESTAMPTZ NULL,
    revoked_at TIMESTAMPTZ NULL,
    last_used_at TIMESTAMPTZ NULL
)
"#,
    r#"
CREATE TABLE IF NOT EXISTS token_claims (
    id BIGSERIAL PRIMARY KEY,
    token_id TEXT NOT NULL REFERENCES tokens(id) ON DELETE CASCADE,
    action TEXT NOT NULL,
    scope TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (token_id, action, scope)
)
"#,
    r#"
CREATE TABLE IF NOT EXISTS packages (
    id BIGSERIAL PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    scope TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
)
"#,
    r#"
CREATE TABLE IF NOT EXISTS package_versions (
    id BIGSERIAL PRIMARY KEY,
    package_id BIGINT NOT NULL REFERENCES packages(id) ON DELETE CASCADE,
    version TEXT NOT NULL,
    status TEXT NOT NULL,
    object_key TEXT NULL,
    integrity TEXT NULL,
    shasum TEXT NULL,
    size_bytes BIGINT NULL,
    manifest JSONB NOT NULL,
    publisher_token_id TEXT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    published_at TIMESTAMPTZ NULL,
    UNIQUE (package_id, version)
)
"#,
    r#"
CREATE TABLE IF NOT EXISTS dist_tags (
    package_id BIGINT NOT NULL REFERENCES packages(id) ON DELETE CASCADE,
    tag TEXT NOT NULL,
    version TEXT NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (package_id, tag)
)
"#,
    r#"
CREATE TABLE IF NOT EXISTS registry_events (
    id BIGSERIAL PRIMARY KEY,
    event_type TEXT NOT NULL,
    actor_token_id TEXT NULL,
    package_name TEXT NULL,
    package_version TEXT NULL,
    payload JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
)
"#,
    "CREATE INDEX IF NOT EXISTS idx_token_claims_token_id ON token_claims(token_id)",
    "CREATE INDEX IF NOT EXISTS idx_packages_scope ON packages(scope)",
    "CREATE INDEX IF NOT EXISTS idx_package_versions_package_id ON package_versions(package_id)",
    "CREATE INDEX IF NOT EXISTS idx_package_versions_ready ON package_versions(package_id, version) WHERE status = 'ready'",
];
