DROP INDEX linked_account_unique_idx;

CREATE UNIQUE INDEX linked_account_unique_idx ON linked_account (owner_id, source_site, lower(username));
