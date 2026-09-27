-- Device key approval (spec §4.1, §6.1): one live request per device, kept
-- on the device row. `approval_public_key` is the pending device's X25519
-- public key and `approval_wrapped` the 92-byte blob an unlocked device
-- wrapped the account key into; both are client material stored as sent,
-- not sealed, and the server cannot open the blob. `approval_expires` is
-- 10 minutes after registration (and again after approval); retention nulls
-- expired requests, a re-sign-in or `DELETE /auth/approval` clears them.

ALTER TABLE devices
    ADD COLUMN approval_public_key  BYTEA,
    ADD COLUMN approval_requested   BIGINT,
    ADD COLUMN approval_expires     BIGINT,
    ADD COLUMN approval_wrapped     BYTEA,
    ADD COLUMN approval_approved_by TEXT,
    ADD COLUMN approval_approved    BIGINT;
