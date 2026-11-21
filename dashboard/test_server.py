import json
import sqlite3
import tempfile
import threading
import unittest
import urllib.request
from pathlib import Path
from http.server import ThreadingHTTPServer
from server import handler

class DashboardTest(unittest.TestCase):
    def setUp(self):
        self.folder=tempfile.TemporaryDirectory()
        self.path=Path(self.folder.name)/'test.db'
        with sqlite3.connect(self.path) as db:
            for migration in sorted((Path(__file__).resolve().parent.parent/'migrations').glob('*.sql')):
                db.executescript(migration.read_text())
            report=json.dumps({'facts':{'token':'0xtest'},'assessment':{'score':50}})
            db.execute("INSERT INTO tokens(token,pair,block,name,symbol,score,level,honeypot,liquidity_bnb,report) VALUES ('0xtest','pair',1,'=DANGER','TST',50,'Medium',0,1,?)",(report,))
        self.server=ThreadingHTTPServer(('127.0.0.1',0),handler(self.path))
        self.thread=threading.Thread(target=self.server.serve_forever,daemon=True);self.thread.start()
        self.base=f'http://127.0.0.1:{self.server.server_port}'
    def tearDown(self):
        self.server.shutdown();self.server.server_close();self.thread.join();self.folder.cleanup()
    def get(self,path):
        with urllib.request.urlopen(self.base+path) as r:return r.read()
    def test_readonly_api_and_history(self):
        self.assertEqual(json.loads(self.get('/api/tokens'))[0]['assessment']['score'],50)
        self.assertEqual(len(json.loads(self.get('/api/history?token=0xtest'))),1)
        self.assertEqual(json.loads(self.get('/api/status'))['pending_alerts'],0)
        self.assertEqual(json.loads(self.get('/api/holders?token=0xtest'))['status'],'not_indexed')
    def test_csv_formula_escaped(self):
        self.assertIn("'=DANGER",self.get('/export.csv').decode())
    def test_dashboard_available(self):
        self.assertIn(b'RugRadar',self.get('/'))
if __name__=='__main__':unittest.main()
