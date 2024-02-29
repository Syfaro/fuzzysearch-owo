CREATE TABLE webauthn_credential (
    id uuid PRIMARY KEY NOT NULL DEFAULT gen_random_uuid(),
    owner_id uuid NOT NULL REFERENCES user_account (id) ON DELETE CASCADE,
    created_at timestamp with time zone NOT NULL DEFAULT current_timestamp,
    credential_id bytea NOT NULL UNIQUE,
    credential jsonb NOT NULL
);

CREATE INDEX webauthn_credential_owner_idx ON webauthn_credential (owner_id, created_at DESC);
