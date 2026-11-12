#!/usr/bin/env python3
"""Run one shard of a set through the verify harness; write numeric JSON only."""
import argparse, glob, json, os, subprocess, sys, tempfile
VERIFY = '.claude/skills/sutura-verify/scripts/verify_default_vs_optout.py'
def ids(path):
    out=[]
    with open(path) as f:
        for line in f:
            line=line.strip()
            if line: out.append(line)
    return out
def main():
    ap=argparse.ArgumentParser()
    ap.add_argument('--set',required=True); ap.add_argument('--shard',type=int,required=True)
    ap.add_argument('--shards',type=int,required=True); ap.add_argument('--mode',required=True)
    ap.add_argument('--out',required=True)
    a=ap.parse_args()
    root=os.getcwd(); corpus=os.path.join(root,'corpus')
    sets_dir=os.path.join(corpus,'sets')
    files=ids(os.path.join(sets_dir,a.set+'.txt'))
    slice_=files[a.shard::a.shards]
    cand=os.path.join(root,'build-cand','bin','python')
    base=os.path.join(root,'build-base','bin','python')
    flags={'byte':['--byte','--deterministic'],'determinism':['--deterministic','--exclude-nondeterministic'],
           'quality_ab':['--quality','--no-visual'],'single_side':['--quality','--no-visual']}.get(a.mode,['--quality','--no-visual'])
    outdir=tempfile.mkdtemp(dir=root)
    cmd=[sys.executable,VERIFY]+[os.path.join(corpus,f) for f in slice_]+flags+['--out-dir',outdir]
    if a.mode!='single_side':
        cmd+=['--repo',root,'--python',cand,'--baseline-repo',root,'--baseline-python',base]
    env=dict(os.environ)
    for k in ('ENV_BASE','ENV_CAND'):
        for kv in (env.pop(k,'') or '').split():
            if '=' in kv: env[kv.split('=',1)[0]]=kv.split('=',1)[1]
    r=subprocess.run(cmd,env=env,capture_output=True,text=True)
    rows=[]
    for jp in glob.glob(os.path.join(outdir,'**','*.json'),recursive=True):
        try: d=json.load(open(jp))
        except Exception: continue
        rows.append({'id':os.path.basename(jp)[:-5],'verdict':d.get('verdict') or d.get('status') or 'UNKNOWN',
                     'metrics':{k:d.get(k) for k in ('holes','nm','components','si','i2o_max','i2o_p95','fidelity','health','risk','volume')}})
    json.dump({'set':a.set,'shard':a.shard,'shards':a.shards,'mode':a.mode,'rc':r.returncode,
               'meshes':len(slice_),'rows':rows},open(a.out,'w'),indent=1)
    sys.exit(0 if r.returncode==0 else 1)
if __name__=='__main__': main()
