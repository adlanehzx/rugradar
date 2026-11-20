CREATE TABLE alert_outbox (
    token TEXT PRIMARY KEY REFERENCES tokens(token),
    message TEXT NOT NULL,
    delivered INTEGER NOT NULL DEFAULT 0
);
