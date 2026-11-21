#!/usr/bin/env python3
"""Replay independently sourced observations with exact block and call parameters.
Manifest entries: source URL, label, block, rpc_method, params, expected_result.
No sample scams are invented. Archive support may be necessary.
"""
import argparse,json,os
from index_holders import rpc
from pathlib import Path

def validate(entry):
    if not entry.get('source','').startswith('https://'):raise ValueError('Independent source URL required')
    if entry.get('rpc_method') not in ('eth_call','eth_getCode','eth_getStorageAt'):raise ValueError('Read-only replay only')
    if not isinstance(entry.get('block'),int) or entry['block']<1:raise ValueError('Exact block required')
    if not entry.get('params') or entry['params'][-1]!=hex(entry['block']):raise ValueError('Params must pin the stated block')
    if 'expected_result' not in entry:raise ValueError('Expected result required')
if __name__=='__main__':
    parser=argparse.ArgumentParser();parser.add_argument('manifest');args=parser.parse_args()
    settings=dict(l.split('=',1) for l in Path('.env').read_text().splitlines() if '=' in l and not l.startswith('#')) if Path('.env').exists() else {}
    results=[]
    for entry in json.loads(Path(args.manifest).read_text()):
        validate(entry)
        try:
            actual=rpc(os.environ.get('RPC_URL',settings.get('RPC_URL','')),entry['rpc_method'],entry['params'])
            results.append({'label':entry.get('label'),'source':entry['source'],'status':'match' if actual==entry['expected_result'] else 'mismatch','actual':actual})
        except Exception:results.append({'label':entry.get('label'),'status':'unavailable'})
    print(json.dumps({'observations':results,'security_accuracy_measured':False},indent=2))
