CREATE TABLE auth_state (
    id uuid PRIMARY KEY default gen_random_uuid(),
    owner_id uuid NOT NULL REFERENCES user_account (id),
    expires_after timestamp with time zone NOT NULL DEFAULT current_timestamp + interval '1 hour',
    state text NOT NULL
);

CREATE UNIQUE INDEX auth_state_user_state_idx ON auth_state (owner_id, state);
