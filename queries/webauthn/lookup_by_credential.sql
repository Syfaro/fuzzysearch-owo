SELECT
    owner_id,
    credential
FROM
    webauthn_credential
WHERE
    webauthn_credential.credential_id = $1;
