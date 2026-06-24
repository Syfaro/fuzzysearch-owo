CREATE TABLE bluesky_repo (
    id integer GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    did text NOT NULL UNIQUE
);

INSERT INTO bluesky_repo (did) SELECT DISTINCT repo FROM bluesky_post;
