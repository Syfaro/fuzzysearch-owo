UPDATE
    user_account
SET
    email_verifier = null
WHERE
    id = $1
    AND email_verifier = $2 RETURNING id;
