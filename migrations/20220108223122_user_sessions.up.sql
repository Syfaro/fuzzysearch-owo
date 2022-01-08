CREATE TABLE user_session (
    id uuid NOT NULL DEFAULT gen_random_uuid(),
    user_id uuid NOT NULL REFERENCES user_account (id),
    created_at timestamp with time zone NOT NULL DEFAULT current_timestamp,
    last_used timestamp with time zone NOT NULL DEFAULT current_timestamp,
    source jsonb NOT NULL,
    PRIMARY KEY (id, user_id)
);

CREATE INDEX account_session_user_id ON user_session (user_id);
