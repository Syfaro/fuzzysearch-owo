INSERT INTO
    user_event (
        owner_id,
        related_to_media_item_id,
        message,
        event_name,
        data,
        created_at
    )
VALUES
    ($1, $2, $3, $4, $5, $6) RETURNING id;
