import subprocess,time,json,hashlib,sys,os
from pathlib import Path
root=Path(__file__).resolve().parents[3]
out=Path(__file__).parent
source=subprocess.check_output(['git','rev-parse','HEAD'],cwd=root,text=True).strip()
paths=subprocess.check_output(['git','ls-files'],cwd=root,text=True).splitlines()
manifest={n:hashlib.sha256((root/n).read_bytes()).hexdigest() for n in paths if (root/n).is_file() and (n.startswith('src/') or n.startswith('loop_v1/') or n.startswith('docs/SPEC'))}
(out/'source-identities.json').write_text(json.dumps({'source_head':source,'files':manifest,'source_scope':'production/tests/spec before full gate'},indent=2)+'\n')
env=os.environ.copy();env['PHONEFARM_DRIVER_GOL_BIN']=str(root/'tasks/control_probe_20261012/accepted-tool/game_opt_loop');env['PHONEFARM_DRIVER_TEST_ARTIFACT_DIR']=str(out/'driver-artifacts');env['PHONEFARM_CONFIRMATION_TEST_ARTIFACT_DIR']=str(out/'observation-artifacts')
receipts=[]
for label,argv in [('cargo-test',['cargo','test','--offline','--manifest-path','src/Cargo.toml','--','--test-threads=4']),('cargo-clippy',['cargo','clippy','--offline','--manifest-path','src/Cargo.toml'])]:
 started=time.time();t=time.monotonic()
 with (out/(label+'.log')).open('wb') as log:
  r=subprocess.run(argv,cwd=root,env=env,stdout=log,stderr=subprocess.STDOUT,timeout=1800)
 receipts.append({'argv':argv,'cwd':str(root),'source_head':source,'started_unix_s':started,'finished_unix_s':time.time(),'elapsed_s':time.monotonic()-t,'exit_code':r.returncode,'device_executed':False,'api_calls':0,'gol_sha256':hashlib.sha256(Path(env['PHONEFARM_DRIVER_GOL_BIN']).read_bytes()).hexdigest()})
 (out/'commands.json').write_text(json.dumps(receipts,indent=2)+'\n')
 if r.returncode:sys.exit(r.returncode)
current={n:hashlib.sha256((root/n).read_bytes()).hexdigest() for n in manifest}
binary=root/'src/target/debug/phonefarm'
(out/'result.json').write_text(json.dumps({'source_head':source,'source_unchanged':current==manifest,'phonefarm_sha256':hashlib.sha256(binary.read_bytes()).hexdigest(),'gol_sha256':hashlib.sha256(Path(env['PHONEFARM_DRIVER_GOL_BIN']).read_bytes()).hexdigest(),'statuses':[r['exit_code'] for r in receipts],'device_executed':False,'api_calls':0},indent=2)+'\n')
if current!=manifest:sys.exit(2)
