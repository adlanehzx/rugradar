ALTER TABLE tokens ADD COLUMN next_check INTEGER NOT NULL DEFAULT 0;
CREATE INDEX tokens_next_check ON tokens(next_check);
