SELECT
    created_at,
    credential_id
FROM
    webauthn_credential
WHERE
    owner_id = $1;
