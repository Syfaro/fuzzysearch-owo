INSERT INTO
    webauthn_credential (owner_id, credential_id, name, credential)
VALUES
    ($1, $2, $3, $4) RETURNING id;
