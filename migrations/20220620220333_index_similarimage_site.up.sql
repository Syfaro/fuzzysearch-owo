CREATE INDEX user_event_similarimage_site_idx ON user_event (
    owner_id,
    event_name,
    (data->'SimilarImage'->>'site'),
    last_updated DESC
);
