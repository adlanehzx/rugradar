#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
root="$(pwd)"
CARGO_BUILD_JOBS=1 CARGO_PROFILE_DEV_DEBUG=0 cargo build --locked
mkdir -p "$HOME/.config/systemd/user"
cat > "$HOME/.config/systemd/user/rugradar.service" <<UNIT
[Unit]
Description=RugRadar BSC scanner and Telegram bot
After=network-online.target
[Service]
WorkingDirectory=$root
ExecStart=$root/target/debug/rugradar
Restart=on-failure
RestartSec=10
TimeoutStopSec=20
UMask=0077
[Install]
WantedBy=default.target
UNIT
cat > "$HOME/.config/systemd/user/rugradar-dashboard.service" <<UNIT
[Unit]
Description=RugRadar local dashboard
[Service]
WorkingDirectory=$root
ExecStart=/usr/bin/python3 $root/dashboard/server.py
Restart=on-failure
RestartSec=5
UMask=0077
[Install]
WantedBy=default.target
UNIT
cat > "$HOME/.config/systemd/user/rugradar-backup.service" <<UNIT
[Unit]
Description=RugRadar SQLite backup
[Service]
WorkingDirectory=$root
ExecStart=/usr/bin/python3 $root/scripts/backup.py
UMask=0077
UNIT
cat > "$HOME/.config/systemd/user/rugradar-backup.timer" <<'UNIT'
[Unit]
Description=Daily RugRadar backup
[Timer]
OnCalendar=daily
Persistent=true
[Install]
WantedBy=timers.target
UNIT
# Stop only the legacy process belonging to this checkout.
python3 - <<'PY'
from pathlib import Path
import os,signal
p=Path('rugradar.pid')
if p.exists():
 try:
  pid=int(p.read_text())
  if Path(f'/proc/{pid}/cwd').resolve()==Path.cwd() and Path(f'/proc/{pid}/comm').read_text().strip()=='rugradar':os.kill(pid,signal.SIGTERM)
 except (FileNotFoundError,ProcessLookupError):pass
PY
systemctl --user daemon-reload
systemctl --user enable --now rugradar.service rugradar-dashboard.service rugradar-backup.timer
