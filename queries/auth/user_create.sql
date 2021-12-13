INSERT INTO
    user_account (username, hashed_password)
VALUES
    ($1, $2) RETURNING id;
