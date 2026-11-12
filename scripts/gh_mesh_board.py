#!/usr/bin/env python3
"""Aggregate shard JSON into the board; --check exits non-zero on worse/fail."""
import glob, json, sys
import argparse
ap=argparse.ArgumentParser(); ap.add_argument('dir'); ap.add_argument('--check',action='store_true')
a=ap.parse_args(); d=a.dir; check=a.check
rows=[]
for p in glob.glob(d+'/*.json'):
    rows.extend(json.load(open(p)).get('rows',[]))
bad=[r for r in rows if str(r.get('verdict','')).upper() in ('WORSE','FAIL','MISSING')]
print('| public ID | verdict | holes | NM | components | SI | i2o max | i2o p95 | Fidelity | Health | Risk |')
print('|---|---|---|---|---|---|---|---|---|---|---|')
for r in sorted(rows,key=lambda x:x['id']):
    m=r.get('metrics',{}) or {}
    print('| %s | %s | %s | %s | %s | %s | %s | %s | %s | %s | %s |'%(
        r['id'],r['verdict'],m.get('holes'),m.get('nm'),m.get('components'),m.get('si'),
        m.get('i2o_max'),m.get('i2o_p95'),m.get('fidelity'),m.get('health'),m.get('risk')))
print('\nRows: %d; worse/fail/missing: %d'%(len(rows),len(bad)))
if check and bad: sys.exit(1)
