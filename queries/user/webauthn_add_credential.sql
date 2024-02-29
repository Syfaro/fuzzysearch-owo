INSERT INTO
    webauthn_credential (owner_id, credential_id, credential)
VALUES
    ($1, $2, $3) RETURNING id;
