#!/usr/bin/env python3
"""Reconstruct address candidates from creation and reconcile balances at one block."""
import argparse,json,os,urllib.request,re
from pathlib import Path
TRANSFER='0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef'
def rpc(url,method,params):
    request=urllib.request.Request(url,data=json.dumps({'jsonrpc':'2.0','id':1,'method':method,'params':params}).encode(),headers={'Content-Type':'application/json'})
    with urllib.request.urlopen(request,timeout=10) as response:body=json.load(response)
    if 'error' in body:raise ValueError('RPC rejected '+method)
    return body['result']
def collect(url,token,start,max_blocks=50000):
    if not re.fullmatch(r'0x[0-9a-fA-F]{40}',token) or start<1:raise ValueError('Invalid token or deployment block')
    head=int(rpc(url,'eth_blockNumber',[]),16)
    if head<start or head-start>max_blocks:raise ValueError('Range exceeds budget; use an archival indexer for older tokens')
    if rpc(url,'eth_getCode',[token,hex(start-1)]) not in ('0x','0x0'):raise ValueError('Contract existed before supplied deployment block')
    code=rpc(url,'eth_getCode',[token,hex(start)])
    if code in ('0x','0x0'):raise ValueError('No contract at supplied deployment block')
    addresses=set()
    for block in range(start,head+1,100):
        logs=rpc(url,'eth_getLogs',[{'address':token,'fromBlock':hex(block),'toBlock':hex(min(head,block+99)),'topics':[TRANSFER]}])
        for log in logs:
            topics=log.get('topics',[])
            if len(topics)!=3:raise ValueError('Malformed transfer event')
            for topic in topics[1:]:
                if not re.fullmatch(r'0x[0-9a-fA-F]{64}',topic):raise ValueError('Malformed transfer address')
                address='0x'+topic[-40:]
                if int(address,16):addresses.add(address.lower())
        if len(addresses)>10000:raise ValueError('Address budget exceeded; no complete report emitted')
    def call(data):return int(rpc(url,'eth_call',[{'to':token,'data':data},hex(head)]),16)
    supply=call('0x18160ddd');holders=[]
    for address in sorted(addresses):
        balance=call('0x70a08231'+address[2:].rjust(64,'0'))
        if balance:holders.append({'address':address,'balance_raw':str(balance),'supply_percent':100*balance/supply if supply else None})
    holders.sort(key=lambda x:int(x['balance_raw']),reverse=True)
    total=sum(int(h['balance_raw']) for h in holders)
    return {'token':token,'from_block':start,'block':head,'candidate_method':'Transfer logs since verified creation block','supply_raw':str(supply),'sum_balances_raw':str(total),'supply_reconciled':total==supply and supply>0,'holders':holders,'security_complete':False,'limitations':['Relies on truthful standard ERC20 events, totalSupply and balanceOf','No inference of wallet relationships or beneficial ownership','Rebases, concealed balances, RPC omissions and reorgs may invalidate reconstruction']}
if __name__=='__main__':
    parser=argparse.ArgumentParser();parser.add_argument('token');parser.add_argument('deployment_block',type=int);args=parser.parse_args()
    settings=dict(l.split('=',1) for l in Path('.env').read_text().splitlines() if '=' in l and not l.startswith('#')) if Path('.env').exists() else {}
    try:print(json.dumps(collect(os.environ.get('RPC_URL',settings.get('RPC_URL','')),args.token,args.deployment_block),ensure_ascii=False))
    except Exception:raise SystemExit('Indexation incomplete: verify deployment block, archive support, RPC availability and range budgets')
