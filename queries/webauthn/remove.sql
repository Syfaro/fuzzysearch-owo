DELETE FROM
    webauthn_credential
WHERE
    owner_id = $1
    AND credential_id = $2;
