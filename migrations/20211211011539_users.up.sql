CREATE TABLE user_account (
    id uuid PRIMARY KEY NOT NULL DEFAULT gen_random_uuid(),
    username text NOT NULL,
    hashed_password text NOT NULL
);

CREATE UNIQUE INDEX user_username_lower_idx ON user_account (lower(username));
