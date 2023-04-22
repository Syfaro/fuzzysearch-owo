ALTER TABLE
    linked_account
ADD
    COLUMN disabled BOOLEAN NOT NULL DEFAULT false;
