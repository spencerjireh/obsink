-- ObSink server schema. Sensitive columns are AES-GCM sealed with the server
-- key (`*_enc`); lookups go through keyed-HMAC index columns (`*_hmac`).
-- `files.path` / `hash` / `enc_path` are already client-side HMAC / ciphertext
-- (spec §6), so they are stored as-is.

CREATE TABLE users (
    id              TEXT PRIMARY KEY,
    email_enc       BYTEA,
    email_hmac      BYTEA UNIQUE,
    apple_sub_enc   BYTEA,
    apple_sub_hmac  BYTEA UNIQUE,
    created         BIGINT NOT NULL
);

CREATE TABLE sessions (
    id              TEXT PRIMARY KEY,
    user_id         TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    token_hash      BYTEA NOT NULL UNIQUE,
    device_name_enc BYTEA NOT NULL,
    created         BIGINT NOT NULL,
    expires         BIGINT NOT NULL
);
CREATE INDEX sessions_user_id_idx ON sessions(user_id);

CREATE TABLE email_codes (
    email_hmac      BYTEA PRIMARY KEY,
    code_hmac       BYTEA,
    expires         BIGINT NOT NULL,
    attempts        INTEGER NOT NULL DEFAULT 0,
    last_sent       BIGINT NOT NULL
);

CREATE TABLE invites (
    code            TEXT PRIMARY KEY,
    created_by      TEXT REFERENCES users(id) ON DELETE CASCADE,
    created         BIGINT NOT NULL,
    expires         BIGINT NOT NULL,
    used_by         TEXT REFERENCES users(id) ON DELETE SET NULL,
    used_at         BIGINT
);
CREATE INDEX invites_created_by_idx ON invites(created_by);

CREATE TABLE vaults (
    id              TEXT PRIMARY KEY,
    tenant          TEXT NOT NULL,
    name_enc        BYTEA NOT NULL,
    created         BIGINT NOT NULL,
    max_file_size   BIGINT NOT NULL,
    revision        BIGINT NOT NULL DEFAULT 0
);
CREATE INDEX vaults_tenant_idx ON vaults(tenant);

CREATE TABLE files (
    vault_id        TEXT NOT NULL REFERENCES vaults(id) ON DELETE CASCADE,
    path            TEXT NOT NULL,
    hash            TEXT NOT NULL,
    modified        BIGINT NOT NULL,
    size            BIGINT NOT NULL,
    deleted         BOOLEAN NOT NULL DEFAULT FALSE,
    enc_path        TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (vault_id, path)
);
