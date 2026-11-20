ALTER TABLE alert_outbox ADD COLUMN cancelled INTEGER NOT NULL DEFAULT 0;
UPDATE alert_outbox SET cancelled=1 WHERE delivered=0 AND token IN
 (SELECT token FROM tokens WHERE COALESCE(json_extract(report,'$.assessment.scoring_version'),0)<2);
