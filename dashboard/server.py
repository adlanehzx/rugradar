#!/usr/bin/env python3
"""Local read-only dashboard. No third-party dependencies."""
import argparse
import csv
import io
import json
import time
import sqlite3
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import urlparse, parse_qs


def handler(database):
    class Handler(BaseHTTPRequestHandler):
        def do_GET(self):
            if self.headers.get('Host','').split(':')[0] not in ('127.0.0.1','localhost'):
                self.send_error(403); return
            url = urlparse(self.path)
            try:
                with sqlite3.connect(f'file:{database}?mode=ro', uri=True) as db:
                    db.row_factory = sqlite3.Row
                    if url.path == '/api/tokens':
                        rows = db.execute('SELECT report, analysed_at FROM tokens ORDER BY analysed_at DESC LIMIT 2000').fetchall()
                        return self.reply([dict(json.loads(r['report']), analysed_at=r['analysed_at']) for r in rows])
                    if url.path == '/api/history':
                        token = parse_qs(url.query).get('token', [''])[0]
                        rows = db.execute('SELECT created_at,report FROM history WHERE token=? ORDER BY id DESC LIMIT 200', (token,)).fetchall()
                        return self.reply([{'date': r['created_at'], 'report': json.loads(r['report'])} for r in rows])
                    if url.path == '/api/status':
                        cursor = db.execute('SELECT last_block FROM cursor WHERE id=1').fetchone()
                        head = db.execute("SELECT value FROM settings WHERE key='chain_head'").fetchone()
                        heartbeat = db.execute("SELECT value FROM settings WHERE key='scanner_heartbeat'").fetchone()
                        return self.reply({'scanner_stale': not heartbeat or time.time()-int(heartbeat[0])>120,
                            'last_block': cursor[0] if cursor else None, 'chain_head': int(head[0]) if head else None,
                            'lag_blocks': max(0,int(head[0])-cursor[0]) if head and cursor else None,
                            'pending_alerts': db.execute('SELECT COUNT(*) FROM alert_outbox WHERE delivered=0 AND cancelled=0').fetchone()[0] + db.execute('SELECT COUNT(*) FROM change_alerts WHERE delivered=0').fetchone()[0],
                            'watchlist': [r[0] for r in db.execute('SELECT token FROM watchlist')],
                            'latest_analysis': db.execute('SELECT MAX(analysed_at) FROM tokens').fetchone()[0]})
                    if url.path == '/export.csv':
                        rows = db.execute('SELECT token,name,symbol,score,level,liquidity_bnb,analysed_at FROM tokens ORDER BY analysed_at DESC').fetchall()
                        output = io.StringIO(); writer = csv.writer(output)
                        writer.writerow(['token','name','symbol','score','level','liquidity_bnb','analysed_at'])
                        # Prevent formula interpretation when exported data is opened in a spreadsheet.
                        for row in rows:
                            writer.writerow(["'"+v if isinstance(v,str) and v.startswith(('=','+','-','@','\t','\r')) else v for v in row])
                        return self.raw(output.getvalue().encode(), 'text/csv; charset=utf-8')
                if url.path == '/':
                    return self.raw(Path(__file__).with_name('index.html').read_bytes(), 'text/html; charset=utf-8')
                self.send_error(404)
            except (sqlite3.Error, ValueError):
                self.send_error(503, 'Database unavailable')

        def reply(self, value):
            self.raw(json.dumps(value).encode(), 'application/json')

        def raw(self, data, content_type):
            self.send_response(200)
            self.send_header('Content-Type', content_type)
            self.send_header('Content-Length', str(len(data)))
            self.send_header('Cache-Control', 'no-store')
            self.send_header('X-Content-Type-Options', 'nosniff')
            self.send_header('Content-Security-Policy', "default-src 'self'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; frame-ancestors 'none'")
            self.end_headers(); self.wfile.write(data)
    return Handler

if __name__ == '__main__':
    parser = argparse.ArgumentParser(); parser.add_argument('--database', default='rugradar.db'); parser.add_argument('--port', type=int, default=8787)
    args = parser.parse_args()
    server = ThreadingHTTPServer(('127.0.0.1', args.port), handler(Path(args.database).resolve()))
    print(f'RugRadar: http://127.0.0.1:{args.port}', flush=True)
    server.serve_forever()
