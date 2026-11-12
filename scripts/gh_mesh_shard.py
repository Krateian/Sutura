#!/usr/bin/env python3
"""One shard: run the verify harness with per-side repos/pythons and real keys."""
import json, os, subprocess, sys, tempfile, traceback
VERIFY = os.environ.get('VERIFY_SCRIPT', 'base/.claude/skills/sutura-verify/scripts/verify_default_vs_optout.py')
KEYS = ('reload_holes','reload_non_manifold_edges','components','closed_shells',
        'orca_number_of_parts','orca_manifold','leak_max_mm','leak_count',
        'two_sided_max_mm','two_sided_p95_mm')
def main():
    set_name=os.environ['SET_NAME']; shard=int(os.environ['SHARD']); shards=int(os.environ['SHARDS'])
    mode=os.environ['MODE']; env_c=os.environ.get('ENV_CAND',''); env_b=os.environ.get('ENV_BASE','')
    if env_b:
        raise SystemExit('ENV_BASE is not supported by the harness (candidate-side env only)')
    corpus=os.path.join(os.getcwd(),'corpus'); sets=os.path.join(corpus,'sets')
    files=[l.strip() for l in open(os.path.join(sets,set_name+'.txt')) if l.strip()]
    files=files[shard::shards]
    flags={'byte':['--byte','--deterministic'],'determinism':['--deterministic','--exclude-nondeterministic'],
           'quality_ab':['--quality','--no-visual'],'single_side':['--quality','--no-visual']}[mode]
    outdir=tempfile.mkdtemp(prefix='shard-')
    cmd=[sys.executable,VERIFY]+[os.path.join(corpus,f) for f in files]+flags+[
         '--baseline-repo',os.path.abspath('base'),'--baseline-python',os.path.abspath('base-venv/bin/python'),
         '--repo',os.path.abspath('cand'),'--python',os.path.abspath('cand-venv/bin/python'),'--out-dir',outdir]
    if env_c: cmd += ['--opt-out-env',env_c]
    r=subprocess.run(cmd,capture_output=True,text=True)
    rows=[]; err=None
    sp=os.path.join(outdir,'summary.json')
    if os.path.exists(sp):
        for item in json.load(open(sp)):
            cand=item.get('candidate') or {}
            metrics={k:cand.get(k) for k in KEYS}
            missing=[k for k,v in metrics.items() if v is None]
            verdict=item.get('verdict') or 'MISSING'
            if missing: verdict='MISSING'
            rows.append({'id':item.get('name'),'verdict':verdict,'reason':item.get('verdict_reason'),
                         'metrics':metrics,'missing':missing})
    else:
        err={'rc':r.returncode,'error_type':'MissingSummary',
             'tail':(r.stderr.strip().splitlines()[-1][:200] if r.stderr.strip() else '')}
    if r.returncode!=0 and err is None:
        err={'rc':r.returncode,'error_type':'HarnessFailed','tail':(r.stderr.strip().splitlines()[-1][:200] if r.stderr.strip() else '')}
    if err:
        for f in files+[outdir]: pass
        json.dump(err,open('error.json','w'),indent=1)
    json.dump({'set':set_name,'shard':shard,'shards':shards,'mode':mode,'rc':r.returncode,
               'meshes':len(files),'rows':rows,'error':err},open('shard-%s-%d.json'%(set_name,shard),'w'),indent=1)
    sys.exit(0 if r.returncode==0 and not err else 1)
if __name__=='__main__':
    try: main()
    except SystemExit: raise
    except Exception as e:
        json.dump({'rc':1,'error_type':type(e).__name__,'tail':''},open('error.json','w')); traceback.print_exc(); sys.exit(1)
