UPDATE
    user_account
SET
    email = $2,
    email_verifier = gen_random_uuid()
WHERE
    id = $1;
