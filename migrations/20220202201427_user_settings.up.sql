CREATE TABLE user_setting (
    owner_id uuid REFERENCES user_account (id),
    setting text NOT NULL,
    value jsonb NOT NULL,
    PRIMARY KEY (owner_id, setting)
);
