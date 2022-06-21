UPDATE
    user_event
SET
    data = jsonb_set(data, '{SimilarImage, site}', '"FList"'::jsonb)
WHERE
    data->'SimilarImage'->>'site' = 'F-list';

UPDATE
    user_event
SET
    data = jsonb_set(data, '{SimilarImage, site}', '"InternalTesting"'::jsonb)
WHERE
    data->'SimilarImage'->>'site' = 'Internal Testing';
