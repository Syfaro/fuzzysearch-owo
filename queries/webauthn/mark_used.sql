UPDATE
    webauthn_credential
SET
    last_used = current_timestamp
WHERE
    credential_id = $1;
