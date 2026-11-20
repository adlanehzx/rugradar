#!/usr/bin/env python3
"""SQLite online backup, bounded retention, private files."""
import os
import sqlite3
from datetime import datetime, timezone
from pathlib import Path
os.umask(0o077)
settings = dict(line.split('=',1) for line in Path('.env').read_text().splitlines() if '=' in line and not line.startswith('#')) if Path('.env').exists() else {}
url = os.environ.get('DATABASE_URL',settings.get('DATABASE_URL','sqlite://rugradar.db'))
if not url.startswith('sqlite://'):raise SystemExit('Expected a SQLite file URL')
source=Path(url.removeprefix('sqlite://')).resolve()
folder=Path('backups');folder.mkdir(exist_ok=True)
backup=folder/(datetime.now(timezone.utc).strftime('%Y%m%dT%H%M%S')+'.db')
with sqlite3.connect(f'file:{source}?mode=ro',uri=True) as src, sqlite3.connect(backup) as dest:
 src.backup(dest)
 if dest.execute('PRAGMA integrity_check').fetchone()[0]!='ok':raise RuntimeError('Backup integrity check failed')
for old in sorted(folder.glob('*.db'))[:-14]:old.unlink()
print('Backup verified:',backup)
