SELECT
    user_account.id,
    webauthn_credential.credential
FROM
    user_account
    JOIN webauthn_credential ON webauthn_credential.owner_id = user_account.id
WHERE
    webauthn_credential.credential_id = $1;
