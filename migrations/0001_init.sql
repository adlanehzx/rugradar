CREATE TABLE IF NOT EXISTS tokens (
    token       TEXT PRIMARY KEY,
    pair        TEXT NOT NULL,
    block       INTEGER NOT NULL,
    name        TEXT NOT NULL,
    symbol      TEXT NOT NULL,
    score       INTEGER NOT NULL,
    level       TEXT NOT NULL,
    honeypot    INTEGER NOT NULL,
    liquidity_bnb REAL NOT NULL,
    report      TEXT NOT NULL,
    alerted     INTEGER NOT NULL DEFAULT 0,
    analysed_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS tokens_score ON tokens (score);
CREATE INDEX IF NOT EXISTS tokens_block ON tokens (block);

CREATE TABLE IF NOT EXISTS cursor (
    id         INTEGER PRIMARY KEY CHECK (id = 1),
    last_block INTEGER NOT NULL
);
