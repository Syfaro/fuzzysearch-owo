UPDATE
    user_event
SET
    data = jsonb_set(data, '{SimilarImage, site}', '"F-list"'::jsonb)
WHERE
    data->'SimilarImage'->>'site' = 'FList';

UPDATE
    user_event
SET
    data = jsonb_set(data, '{SimilarImage, site}', '"Internal Testing"'::jsonb)
WHERE
    data->'SimilarImage'->>'site' = 'InternalTesting';
