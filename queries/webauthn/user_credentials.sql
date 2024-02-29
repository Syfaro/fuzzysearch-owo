SELECT
    created_at,
    name,
    credential_id
FROM
    webauthn_credential
WHERE
    owner_id = $1
ORDER BY
    last_used DESC;
