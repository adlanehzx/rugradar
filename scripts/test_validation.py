import unittest
from unittest.mock import patch
from index_holders import collect,TRANSFER
from replay import validate
TOKEN='0x'+'11'*20
ACCOUNT='0x'+'22'*20
class ValidationTest(unittest.TestCase):
 def fake(self,url,method,params):
  if method=='eth_blockNumber':return '0x64'
  if method=='eth_getBlockByNumber':return {'hash':'0x123'}
  if method=='eth_getCode':return '0x' if params[-1]=='0x62' else '0x6000'
  if method=='eth_getLogs':return [{'topics':[TRANSFER,'0x'+'0'*64,'0x'+ACCOUNT[2:].rjust(64,'0')]}]
  if method=='eth_call':return '0x64'
  raise AssertionError(method)
 def test_reconcile_pinned_balances(self):
  with patch('index_holders.rpc',side_effect=self.fake):
   result=collect('unused',TOKEN,99)
  self.assertTrue(result['supply_reconciled']);self.assertFalse(result['security_complete']);self.assertEqual(result['holders'][0]['address'],ACCOUNT)
 def test_rpc_failure_emits_no_complete_report(self):
  with patch('index_holders.rpc',side_effect=ValueError('unavailable')):
   with self.assertRaises(ValueError):collect('unused',TOKEN,99)
 def test_replay_rejects_unpinned_or_unsourced_case(self):
  with self.assertRaises(ValueError):validate({'source':'https://example.com','block':1,'rpc_method':'eth_call','params':[{},'latest'],'expected_result':'0x'})
if __name__=='__main__':unittest.main()
