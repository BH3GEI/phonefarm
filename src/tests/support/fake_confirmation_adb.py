#!/usr/bin/env python3
"""Explicit simulation only. Implements transport receipt protocol, never real-device data."""
import json, os, pathlib, re, shlex, sys
root=pathlib.Path(os.environ['PHONEFARM_FAKE_DEVICE_DIR']); root.mkdir(parents=True,exist_ok=True)
statepath=root/'fake-state.json'
if statepath.exists(): state=json.loads(statepath.read_text())
else:
    state={'files':{},'runs':0}
    state['snapshot']='''# device_snapshot v1
ftrace.tracing_on=0
ftrace.current_tracer=nop
ftrace.buffer_size_kb=65536
ftrace.set_event=
kgsl.min_pwrlevel=5
kgsl.max_pwrlevel=0
kgsl.default_pwrlevel=0
kgsl.thermal_pwrlevel=0
kgsl.force_clk_on=0
kgsl.force_rail_on=0
kgsl.force_bus_on=0
kgsl.force_no_nap=0
kgsl.idle_timer=80
kgsl.max_gpuclk=1000
kgsl.pwrscale=1
kgsl.bus_split=1
kgsl.perfcounter=1
kgsl.devfreq_governor=msm-adreno-tz
kgsl.devfreq_min_freq=100
kgsl.devfreq_max_freq=1000
bus.DDR.boost_freq=0
bus.DDR.hw_max_freq=1000
bus.DDR.hw_min_freq=100
bus.LLCC.boost_freq=0
bus.LLCC.hw_max_freq=1000
bus.LLCC.hw_min_freq=100
wm.size=Physical size: 1080x2400
wm.density=Physical density: 480
settings.peak_refresh_rate=120
settings.min_refresh_rate=60
settings.refresh_rate_mode=0
cpu.policy0.governor=schedutil
cpu.policy0.scaling_max=1000
cpu.policy0.scaling_min=100
# end
'''
def thermal_hal():
    status='3' if fail=='thermal_severe' else '2'
    override='true' if fail=='thermal_override' else 'false'
    out=f'IsStatusOverride: {override}\nThermal Status: {status}\nHAL Ready: true\n'
    for ty,name,t in [(0,'CPU0',95),(1,'GPU0',95),(2,'battery',80),(3,'skin',60)]:
        out+=f'TemperatureThreshold{{mType={ty}, mName={name}, mHotThrottlingThresholds=[NaN, NaN, NaN, {t}.0, NaN, NaN, 115.0], mColdThrottlingThresholds=[]}}\n'
    return out
def thermal_raw(step=5,up=None):
    up=state.get('thermal_uptime',2000.) if up is None else up;state['thermal_uptime']=up+step
    call=state.get('thermal_calls',0);state['thermal_calls']=call+1
    out=f'#phonefarm_thermal_sample v1\nUPTIME\t{up}\n'
    for path,v in [('/sys/class/thermal/thermal_zone0/temp',80000),('/sys/class/thermal/thermal_zone1/temp',60000),('/sys/class/thermal/thermal_zone2/temp',50000),('/sys/class/power_supply/battery/temp',390)]:
        if 'thermal_zone0' in path and fail in ['capability_fluctuating','thermal_drift_31']:v+=((call%2)* (7700 if fail=='capability_fluctuating' else 3100))
        if fail=='thermal_missing' and 'battery' in path:continue
        if fail=='thermal_hot_before_probe' and state['files'] and 'thermal_zone0' in path:v=92000
        out+=f'TEMP\t{path}\t{v}\n'
    return out+'#hal_begin\n'+thermal_hal()+'#hal_end\n#end\n'
args=sys.argv[1:];assert args[:2]==['-s','fake-target'] and args[2]=='shell'
cmd=shlex.split(args[3]);assert cmd[:2]==['su','-c']; cmd=cmd[2]; data=sys.stdin.buffer.read()
with (root/'argv.jsonl').open('a') as f:f.write(json.dumps({'argv':args,'command':cmd,'stdin':data.decode(),'simulation':True})+'\n')
fail=os.environ.get('PHONEFARM_FAKE_FAIL','')
if fail=='closed_pipes_child' and cmd=='sh' and b'# device_snapshot' in data:
    pid=os.fork()
    if pid==0:
        os.close(1);os.close(2)
        import time;time.sleep(20);os._exit(0)
    sys.exit(0)
if fail=='root' and cmd.startswith('set -eu; id;'):
    print('uid=2000(shell)');print('fake-serial');print('fake-boot');print('/system/bin/setsid');sys.exit(0)
if cmd.startswith('set -eu; id;'):
    print('uid=0(root) gid=0(root)');print('fake-serial');print('changed-boot' if fail=='boot' else 'fake-boot');print('/system/bin/setsid')
elif cmd=='dumpsys activity activities': print('mResumedActivity: ActivityRecord x '+('com.example.crashed/.Home' if fail=='foreground' else 'com.example.game/.Game')+' t1')
elif cmd=='timeout 8 dumpsys thermalservice':sys.stdout.write(thermal_hal())
elif cmd=='sh':
    if b'#phonefarm_thermal_metadata v1' in data:
        print('#phonefarm_thermal_metadata v1\nBOOT\tfake-boot')
        for i,ty,dom,t in [(0,'cpu-0','cpu',108000),(1,'gpuss-0','gpu',95000),(2,'skin-msm-therm','skin',60000)]:
            unit='deci_celsius' if fail=='thermal_wrong_unit' and dom=='cpu' else 'milli_celsius'
            print(f'SENSOR\t/sys/class/thermal/thermal_zone{i}/temp\t{ty}\t{dom}\t{unit}\nTRIP\t/sys/class/thermal/thermal_zone{i}/temp\tpassive\t{t}')
        print('SENSOR\t/sys/class/power_supply/battery/temp\tbattery\tbattery\tdeci_celsius\n#end')
    elif b'#phonefarm_thermal_sample v1' in data:
        count=data.count(b'sh -c') or 1
        if fail=='thermal_short_admission' and count>=6:count=5
        for n in range(count):sys.stdout.write(thermal_raw(5,1045977.+state['runs']*20 if count==1 else None))
        if fail=='loaded_cpu_drift' and count>=6:
            state['snapshot_before_warmup']=state['snapshot']
            state['snapshot']=state['snapshot'].replace('cpu.policy0.scaling_max=1000','cpu.policy0.scaling_max=600')
    elif b'# device_snapshot' in data: sys.stdout.write(state['snapshot'])
    elif b'probe_sysparam' in data:
        print('# probe_sysparam v1\ncpu.policies=0')
        max_freq='600' if 'cpu.policy0.scaling_max=600' in state['snapshot'] else '1000'
        print(f'cpu.policy0.avail_freqs=100 300 600 1000\ncpu.policy0.avail_governors=schedutil performance\ncpu.policy0.scaling_min_freq.cur=100\ncpu.policy0.scaling_max_freq.cur={max_freq}\ncpu.policy0.scaling_governor.cur=schedutil')
        print('gpu.num_pwrlevels=6\ngpu.min_pwrlevel.cur=5\ngpu.max_pwrlevel.cur=0\ngpu.devfreq_path=/sys/class/kgsl/kgsl-3d0/devfreq\ngpu.devfreq.avail_freqs=100 300 600 1000\ngpu.devfreq.avail_governors=msm-adreno-tz performance\ngpu.devfreq.min_freq.cur=100\ngpu.devfreq.max_freq.cur=1000\ngpu.devfreq.governor.cur=msm-adreno-tz')
        for bus in ['DDR','LLCC']:print(f'bus.{bus}.boost_freq.cur=0\nbus.{bus}.hw_min_freq=100\nbus.{bus}.hw_max_freq=1000\nbus.{bus}.avail_freqs=100 300 600 1000')
        print('setting.system.peak_refresh_rate.read_exit=0\nsetting.system.min_refresh_rate.read_exit=0\nsetting.system.refresh_rate_mode.read_exit=0\ndisplay.modes=fps=60,fps=120\nsetting.system.peak_refresh_rate.cur=120\nsetting.system.min_refresh_rate.cur=60\nsetting.system.refresh_rate_mode.cur=0\nsetting.system.all_refresh_rate=60 120\nsetting.system.refresh_rate_mode.base_fps=60\n# end')
    elif b'hot=0' in data:print('cpu-0 45000' if fail=='cold' else 'cpu-0 39000')
    else:raise Exception('unknown stdin script')
elif cmd.startswith('set -eu; umask 077; mkdir '):
    words=shlex.split(cmd); remote=words[5].rstrip(';'); owner=words[8]
    # Extract with actual token following printf format.
    token=re.search(r"printf '%s\\n' '([^']+)'",cmd).group(1)
    state['files'][remote+'/owner']=token+'\n'
elif cmd.startswith('set -eu; test ') and 'cat >' in cmd:
    dst=shlex.split(cmd.split('cat > ',1)[1].split(';',1)[0])[0];state['files'][dst]=data.decode()
elif cmd.startswith('cat '):
    path=shlex.split(cmd)[1]
    sys.stdout.write(state['files'][path])
elif cmd.startswith('setsid sh ') and '/worker.sh ' in cmd:
    words=shlex.split(cmd)[1:]; d,token,boot,mode,run=words[2:7];r=d+'/'+run
    assert state['files'][d+'/owner'].strip()==token+' '+boot
    if d+'/session.closed' in state['files']:sys.exit(86)
    if fail=='probe' and mode in ['probe','control_probe']:sys.exit(74)
    if fail=='workload' and mode=='sample':sys.exit(91)
    if mode in ['probe','control_probe']:
        names=['snapshot_before','snapshot_at_run','snapshot_after_run','snapshot_restored'] if mode=='control_probe' else ['snapshot_before','snapshot_restored']
        for name in names:
            snap=state['snapshot']
            if fail=='restore' and name=='snapshot_restored':snap=snap.replace('kgsl.force_clk_on=0','kgsl.force_clk_on=1')
            state['files'][r+'/'+name+'.txt']=snap;state['files'][r+'/'+name+'.exit']='0\n'
        key,kind,path,val=state['files'][d+'/'+run+'.plan.tsv'].strip().split('\t')
        receipt=f'{key}.writable=yes\n{key}.effect=live\n{key}.effect_restored=yes\n'
        if key=='setting.system.refresh_rate_mode':receipt+=f'{key}.mode{val}_fps=120 (readback={val})\n'
        state['files'][r+'/probe_receipt.txt']=receipt
        if mode=='control_probe' and d+'/thermal_sample.sh' in state['files']:
            if d+'/thermal-purpose.txt' in state['files']:
                start=state.get('thermal_uptime',2000.)
                series=''.join(thermal_raw(1,start+offset) for offset in [0,1.1,6.1,11.1])
                if fail=='control_hold_hot':series=series.replace('\t80000\n','\t92000\n')
                if fail=='control_hold_severe':series=series.replace('Thermal Status: 2','Thermal Status: 3')
                series+=f'#phonefarm_control_hold_complete start={start+.05} end={start+11.15} samples=4 profile=capability_absolute_v1\n'
                if fail=='control_hold_incomplete':series=series.split('#phonefarm_control_hold_complete')[0]
                state['files'][r+'/thermal_hold.raw.tsv']=series
            else:state['files'][r+'/thermal_hold.raw.tsv']=''.join(thermal_raw(5) for _ in range(4))
        print('PROBE_COMPLETE')
    elif mode=='sample':
        duration,lead,envdur=int(words[7]),int(words[8]),int(words[9]);role=words[10]
        base=1045978.0+state['runs']*20;state['runs']+=1
        trace=(root/'trace_fixture.txt').read_text().split('\n',1)[1]
        trace=re.sub(r'10459\d{2}\.\d+',lambda m:f'{float(m[0])+(base-1045978):.6f}',trace)
        state['files'][r+'/trace.txt']=f'#loop_v1_meta capture_start_uptime={base+1.1} capture_end_uptime={base+3.56} duration_req=2\n'+trace
        env=f'#sample_env v1 dur={envdur} interval=1\n#battery_status=Discharging\n#battery_capacity=80\n#fan_state=none\n# fixture-run={run}\n'
        for n in range(envdur):env+=f'ENV {base+n} 3850000 -1000000 NA 0 0 cpu-0 39000\n'
        env+='#battery_status_after=Discharging\n#sample_env done\n'
        if fail=='power':env=env.replace('Discharging','Charging')
        if fail=='env':env=env.replace('#sample_env done\n','')
        state['files'][r+'/sampler.log']=env
        if d+'/thermal_sampler.sh' in state['files']:
            start=base-.5;dur=envdur+3
            series=''.join(thermal_raw(1,start+n) for n in range(dur+1))
            series+=f'#phonefarm_thermal_window_complete dur={dur} interval=1 start={start} end={start+dur+.05} samples={dur+1}\n'
            if fail=='thermal_window_short':series=series.replace('samples=', 'invalid_samples=')
            if fail=='thermal_window_hot':series=series.replace('\t80000\n','\t92000\n')
            state['files'][r+'/thermal_sampler.log']=series
            state['files'][r+'/thermal_sampler_exit.txt']='1\n' if fail=='thermal_sampler_exit' else '0\n'

        for name in ['workload_exit','sampler_exit','capture_exit','restore_exit']:state['files'][r+'/'+name+'.txt']='0\n'
        state['files'][r+'/root_id.txt']='uid=0(root) gid=0(root)\n';state['files'][r+'/boot_id.txt']=boot+'\n'
        active=state['snapshot']
        if role=='candidate':
            for line in state['files'][d+'/candidate.plan.tsv'].splitlines():
                key,kind,path,val=line.split('\t')
                if path.startswith('/sys/devices/system/cpu/cpufreq/'):
                    field={'scaling_governor':'governor','scaling_min_freq':'scaling_min','scaling_max_freq':'scaling_max'}[path.rsplit('/',1)[1]];key='cpu.policy0.'+field
                elif path.startswith('/sys/class/kgsl/kgsl-3d0/devfreq/'):key='kgsl.devfreq_'+path.rsplit('/',1)[1]
                elif path.startswith('/sys/class/kgsl/'):key='kgsl.'+path.rsplit('/',1)[1]
                elif kind=='setting':key='settings.'+path.split(':')[1]
                active=re.sub(re.escape(key)+r'=.*',key+'='+val,active)
        for name,snap in [('snapshot_before',state['snapshot']),('snapshot_at_run',active),('snapshot_after_run',active),('snapshot_restored',state['snapshot'])]:
            if fail=='restore' and name=='snapshot_restored':snap=snap.replace('bus.DDR.boost_freq=0','bus.DDR.boost_freq=300')
            if fail=='active' and name=='snapshot_after_run':snap=snap.replace('kgsl.force_clk_on=0','kgsl.force_clk_on=1')
            state['files'][r+'/'+name+'.txt']=snap;state['files'][r+'/'+name+'.exit']='0\n'
        state['files'][r+'/quiescence.txt']='OWNED_GROUPS_ABSENT\n';print('SAMPLE_COMPLETE')
    elif mode in ['reconcile','reconcile_controls']:
        state['files'][r+'/snapshot_restored.txt']=state['snapshot'];state['files'][r+'/snapshot_restored.exit']='0\n';state['files'][r+'/restore_exit.txt']='0\n';state['files'][r+'/quiescence.txt']='OWNED_GROUPS_ABSENT\n';print('RECONCILE_COMPLETE')
        state['files'][d+'/session.closed']=token+' '+boot+' '+run
        if fail=='closure_unknown':
            statepath.write_text(json.dumps(state));sys.exit(87)
        state['files'][r+'/session_closed.txt']=f'SESSION_REVOKED {token} {boot} {run}\n'
    else:raise Exception('unknown worker mode')
else: raise Exception('unknown command '+cmd)
statepath.write_text(json.dumps(state))
