CREATE TABLE history (
 id INTEGER PRIMARY KEY, token TEXT NOT NULL, report TEXT NOT NULL,
 created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);
CREATE INDEX history_token ON history(token,id);
CREATE TABLE change_alerts (
 id INTEGER PRIMARY KEY, token TEXT NOT NULL, message TEXT NOT NULL,
 delivered INTEGER NOT NULL DEFAULT 0
);
CREATE TRIGGER tokens_history_insert AFTER INSERT ON tokens BEGIN
 INSERT INTO history(token,report) VALUES (NEW.token,NEW.report);
END;
CREATE TRIGGER tokens_history_update AFTER UPDATE OF report ON tokens BEGIN
 INSERT INTO history(token,report) VALUES (NEW.token,NEW.report);
 INSERT INTO change_alerts(token,message)
 SELECT NEW.token, '⚠️ Changement sur un token suivi : ' || NEW.token || char(10) ||
 'Score : ' || OLD.score || ' → ' || NEW.score || char(10) ||
 'Liquidité : ' || OLD.liquidity_bnb || ' → ' || NEW.liquidity_bnb || ' BNB' || char(10) ||
 'Consulte /check ' || NEW.token
 WHERE EXISTS(SELECT 1 FROM watchlist WHERE token=NEW.token) AND (
 (OLD.liquidity_bnb > 0.1 AND NEW.liquidity_bnb < OLD.liquidity_bnb * 0.8)
 OR (NEW.score <= OLD.score - 10)
 OR (NEW.honeypot > OLD.honeypot)
 OR (json_extract(NEW.report,'$.facts.simulation.buy_tax_bps') > json_extract(OLD.report,'$.facts.simulation.buy_tax_bps'))
 OR (json_extract(NEW.report,'$.facts.simulation.sell_tax_bps') > json_extract(OLD.report,'$.facts.simulation.sell_tax_bps'))
 OR (json_extract(NEW.report,'$.facts.ownership') != json_extract(OLD.report,'$.facts.ownership'))
 );
END;
INSERT INTO history(token,report) SELECT token,report FROM tokens;
