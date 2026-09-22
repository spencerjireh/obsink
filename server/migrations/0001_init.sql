-- ObSink server schema (wire format v3, spec §4.2). Sensitive columns are
-- AES-GCM sealed with the server key (`*_enc`); lookups go through keyed-HMAC
-- index columns (`*_hmac`). `files.path` / `hash` / `enc_path` and every
-- wrapped key are already client-side HMAC / ciphertext (spec §6), so they are
-- stored as-is.

CREATE TABLE users (
    id                   TEXT PRIMARY KEY,
    email_enc            BYTEA,
    email_hmac           BYTEA UNIQUE,
    apple_sub_enc        BYTEA,
    apple_sub_hmac       BYTEA UNIQUE,
    created              BIGINT NOT NULL,
    -- The account key wrapped under the passphrase KEK (client ciphertext),
    -- its Argon2id salt, a server-assigned id, and the verifier a rewrap must
    -- present. All null until the first `PUT /auth/keys`.
    account_key_enc      BYTEA,
    account_key_salt     BYTEA,
    account_key_id       TEXT,
    account_key_verifier BYTEA
);

-- A physical machine of an account: the client keeps `id` for good, so a
-- second sign-in from the same machine replaces its session instead of
-- adding a row. Keyed per user so two accounts on one machine never collide.
CREATE TABLE devices (
    user_id         TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    id              TEXT NOT NULL,
    name_enc        BYTEA NOT NULL,
    platform        TEXT NOT NULL,
    created         BIGINT NOT NULL,
    last_seen       BIGINT NOT NULL,
    PRIMARY KEY (user_id, id)
);

CREATE TABLE sessions (
    id              TEXT PRIMARY KEY,
    user_id         TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    device_id       TEXT NOT NULL,
    token_hash      BYTEA NOT NULL UNIQUE,
    created         BIGINT NOT NULL,
    expires         BIGINT NOT NULL,
    FOREIGN KEY (user_id, device_id) REFERENCES devices(user_id, id) ON DELETE CASCADE,
    UNIQUE (user_id, device_id)
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
    owner           TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name_enc        BYTEA NOT NULL,
    created         BIGINT NOT NULL,
    max_file_size   BIGINT NOT NULL,
    revision        BIGINT NOT NULL DEFAULT 0,
    last_write      BIGINT NOT NULL DEFAULT 0
);
CREATE INDEX vaults_owner_idx ON vaults(owner);

-- One row per account that holds the vault key, wrapped under that account's
-- key. v3 writes the owner only; sharing adds members later.
CREATE TABLE vault_members (
    vault_id        TEXT NOT NULL REFERENCES vaults(id) ON DELETE CASCADE,
    user_id         TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    role            TEXT NOT NULL,
    wrapped_key     BYTEA,
    created         BIGINT NOT NULL,
    PRIMARY KEY (vault_id, user_id)
);
CREATE INDEX vault_members_user_id_idx ON vault_members(user_id);

-- Which devices hold a vault and how far each has synced (spec §4.3,
-- `PUT /vaults/:id/devices/self`). Rows go with their device or vault.
CREATE TABLE device_vaults (
    user_id         TEXT NOT NULL,
    device_id       TEXT NOT NULL,
    vault_id        TEXT NOT NULL REFERENCES vaults(id) ON DELETE CASCADE,
    attached        BIGINT NOT NULL,
    last_synced     BIGINT,
    last_revision   BIGINT,
    PRIMARY KEY (user_id, device_id, vault_id),
    FOREIGN KEY (user_id, device_id) REFERENCES devices(user_id, id) ON DELETE CASCADE
);
CREATE INDEX device_vaults_vault_id_idx ON device_vaults(vault_id);

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
