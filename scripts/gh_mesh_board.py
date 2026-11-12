#!/usr/bin/env python3
"""Aggregate shard JSONs into the board; --check fails on worse/fail/missing."""
import argparse, glob, json, sys
ap=argparse.ArgumentParser(); ap.add_argument('dir'); ap.add_argument('--check',action='store_true')
a=ap.parse_args(); rows=[]; errors=[]
for p in glob.glob(a.dir+'/*.json'):
    d=json.load(open(p)); rows.extend(d.get('rows',[]))
    if d.get('error'): errors.append(d['error'])
bad=[r for r in rows if str(r.get('verdict','')).upper() in ('WORSE','FAIL','MISSING','ERROR','VISUAL-WORSE')]
print('| public ID | verdict | holes | NM | components | shells | Orca parts | Orca manifold | leak mm | leak pts | two-sided max | two-sided p95 |')
print('|---|---|---|---|---|---|---|---|---|---|---|---|')
for r in sorted(rows,key=lambda x:str(x['id'])):
    m=r.get('metrics',{})
    def g(k): 
        v=m.get(k); return 'MISSING' if v is None else v
    print('| %s | %s | %s | %s | %s | %s | %s | %s | %s | %s | %s | %s |'%(
        r['id'],r['verdict'],g('reload_holes'),g('reload_non_manifold_edges'),g('components'),
        g('closed_shells'),g('orca_number_of_parts'),g('orca_manifold'),g('leak_max_mm'),
        g('leak_count'),g('two_sided_max_mm'),g('two_sided_p95_mm')))
print('\nRows: %d; worse/fail/missing: %d; shard errors: %d'%(len(rows),len(bad),len(errors)))
if a.check and (bad or errors): sys.exit(1)
