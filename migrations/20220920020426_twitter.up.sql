CREATE TABLE twitter_auth (
    owner_id uuid NOT NULL REFERENCES user_account (id),
    request_key text NOT NULL PRIMARY KEY,
    request_secret text NOT NULL,
    created_at TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT current_timestamp
);
