#!/usr/bin/env python3
"""Observe externally documented adverse tokens. Not an accuracy estimate."""
import json,subprocess,re
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from datetime import datetime,timezone
root=Path(__file__).resolve().parent.parent
cases=json.loads((root/'validation/dead-token-archive.json').read_text())['cases']
def run(case):
    address=case['contract_address'];result={'token':address,'external_status':case.get('status_en'),'external_label_not_independently_adjudicated':True}
    if not re.fullmatch(r'0x[0-9a-fA-F]{40}',address):return dict(result,status='invalid_address')
    try:
        p=subprocess.run([str(root/'target/debug/rugradar'),'check',address],cwd=root,capture_output=True,text=True,timeout=70)
        if p.returncode:return dict(result,status='analysis_unavailable')
        report=json.loads(p.stdout);return dict(result,status='observed',report=report)
    except Exception:return dict(result,status='analysis_unavailable')
with ThreadPoolExecutor(max_workers=2) as pool:results=list(pool.map(run,cases))
output={'run_utc':datetime.now(timezone.utc).isoformat(),'source':'https://github.com/scytheradar/scythe-radar-datasets/tree/2d587e997e319ebac983b2748e44f8d798638439','historical_prediction_test':False,'accuracy_estimated':False,'limitations':['Adverse cases only; no representative benign controls','Current chain state, not state before each incident','Third-party labels retained as assertions, not proven ground truth','Unavailable cases are not successes'],'results':results}
(root/'validation/latest-observations.json').write_text(json.dumps(output,indent=2,ensure_ascii=False))
print('Saved validation/latest-observations.json; no accuracy claim made')
