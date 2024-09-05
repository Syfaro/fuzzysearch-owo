CREATE TABLE site_alert (
    id uuid PRIMARY KEY NOT NULL DEFAULT gen_random_uuid(),
    created_at timestamp with time zone NOT NULL DEFAULT current_timestamp,
    active boolean NOT NULL DEFAULT true,
    content text NOT NULL
);

CREATE INDEX site_alert_lookup_idx ON site_alert (created_at DESC);

CREATE TABLE site_alert_dismiss (
    site_alert_id uuid NOT NULL REFERENCES site_alert (id) ON DELETE CASCADE,
    user_account_id uuid NOT NULL REFERENCES user_account (id) ON DELETE CASCADE,
    dismissed_at timestamp with time zone NOT NULL DEFAULT current_timestamp,
    PRIMARY KEY (site_alert_id, user_account_id)
);
