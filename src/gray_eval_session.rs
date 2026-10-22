//! Owned resources for one gray eval launch route; the base session owns the lease.
use crate::{confirmation_transport::{new_file, quote as q}, gray_session::Session};
use serde_json::{json, Value};
use std::{cell::RefCell, fs, path::Path, time::{Duration, Instant}};

const SCHEMA: &str = "phonefarm/gray-eval-launch-session@1";
const FAN: &str = "/sys/kernel/fan/fan_enable";
const WORKER: &str = include_str!("../loop_v1/tools/confirmation_worker.sh");
const BOOTSTRAP: &str = include_str!("../loop_v1/tools/confirmation_bootstrap.sh");
const TRACE: &str = include_str!("../loop_v1/tools/gray_trace_workload.sh");
const SAMPLER: &str = include_str!("../loop_v1/auto/sample_env.sh");

pub(crate) struct Parent {
    pub base: Session,
    state: RefCell<Value>,
}

impl Parent {
    pub fn new(base: Session, request: &Value) -> Result<Self, String> {
        let id = base.eval_identity()?;
        let token = id["token"].as_str().ok_or("missing token")?;
        let state = json!({"schema":SCHEMA,"identity":id,"stage":"created","request":request,
            "remote":format!("/data/local/tmp/phonefarm-confirmation-gray-{token}"),
            "trace":format!("/sys/kernel/tracing/instances/phonefarm_gray_{token}"),
            "deployment":"not_started","tools_created":false,"trace_state":"not_started","fan":null,"pending":null,"rounds":[]});
        new_file(&base.dir.join("eval-session.json"), &serde_json::to_vec_pretty(&state).unwrap())?;
        Ok(Self {base, state:RefCell::new(state)})
    }

    pub fn resume(dir: &Path, serial: &str) -> Result<Self, String> {
        let base = Session::resume_eval(dir, serial)?;
        let state: Value = serde_json::from_slice(&crate::confirmation_transport::bounded(&base.dir.join("eval-session.json"), 1024*1024)?)
            .map_err(|e| e.to_string())?;
        let id = base.eval_identity()?;
        let token = id["token"].as_str().ok_or("missing token")?;
        if state["schema"] != SCHEMA || ["serial","token","session","evidence_origin"].iter().any(|k|state["identity"][*k]!=id[*k])
            || state["remote"] != format!("/data/local/tmp/phonefarm-confirmation-gray-{token}")
            || state["trace"] != format!("/sys/kernel/tracing/instances/phonefarm_gray_{token}")
            || !matches!(state["deployment"].as_str(),Some("not_started"|"installing"|"ready"))
            || !matches!(state["trace_state"].as_str(),Some("not_started"|"created"|"removed"))
            || !state["tools_created"].is_boolean()
            || ["boot_id","native_dir"].iter().any(|k|!state["identity"][*k].is_null()&&state["identity"][*k]!=id[*k])
            || state.get("pending").is_none() || !state["pending"].is_null()
        { return Err("invalid or pending parent gray eval resources; independent reconciliation required".into()); }
        if !state["fan"].is_null() && (!matches!(state["fan"]["original"].as_str(),Some("0"|"1")) || !state["fan"]["changed"].is_boolean()) {
            return Err("invalid original fan state".into());
        }
        Ok(Self {base,state:RefCell::new(state)})
    }

    fn update(&self, f: impl FnOnce(&mut Value)) -> Result<(), String> {
        let mut next = self.state.borrow().clone(); f(&mut next);
        crate::devlease::durable_replace(&self.base.dir.join("eval-session.json"), &serde_json::to_vec_pretty(&next).unwrap())?;
        *self.state.borrow_mut()=next; Ok(())
    }
    pub fn stage(&self, stage: &str) -> Result<(),String> { self.update(|s|s["stage"]=json!(stage)) }
    fn remote(&self)->String { self.state.borrow()["remote"].as_str().unwrap().into() }
    fn trace(&self)->String { self.state.borrow()["trace"].as_str().unwrap().into() }
    fn clear(&self)->Result<(),String> {
        self.base.eval_ready()?;
        if !self.state.borrow()["pending"].is_null() {return Err("parent resource mutation completion is unrecorded".into());} Ok(())
    }
    pub fn read(&self, op:&str, command:&str)->Result<String,String> {
        self.clear()?;
        Ok(self.base.eval_transport(op,&["shell".into(),format!("su -c {}",q(command))],30_000,false)?.stdout)
    }
    fn mutate(&self, op:&str, args:&[String], ms:u64, done:impl FnOnce(&mut Value))->Result<String,String> {
        self.clear()?;
        self.update(|s|s["pending"]=json!(op))?;
        let result=self.base.eval_transport(op,args,ms,true)?;
        self.update(|s|{done(s);s["pending"]=Value::Null;})?;
        Ok(result.stdout)
    }
    fn write(&self, op:&str, cmd:&str, done:impl FnOnce(&mut Value))->Result<String,String> {
        self.mutate(op,&["shell".into(),format!("su -c {}",q(cmd))],30_000,done)
    }
    fn owned(&self)->Result<String,String> {
        let id=self.base.eval_identity()?;
        let boot=id["boot_id"].as_str().ok_or("missing boot identity")?;
        Ok(format!("test \"$(cat {}/owner)\" = {} && test \"$(cat /proc/sys/kernel/random/boot_id)\" = {}",
            q(&self.remote()),q(&format!("{} {boot}",id["token"].as_str().unwrap())),q(boot)))
    }
    pub fn capabilities(&self)->Result<crate::qualification::Capabilities,String> {
        let uid=self.read("eval_root","id -u")?;
        let available=self.read("eval_trace_capability","if [ -r /sys/kernel/tracing/events/kgsl/adreno_cmdbatch_submitted/format ] && [ -r /sys/kernel/tracing/events/kgsl/adreno_cmdbatch_retired/format ] && [ -d /sys/kernel/tracing/instances ]; then echo AVAILABLE; else echo UNAVAILABLE; fi")?;
        Ok(crate::qualification::Capabilities {backend:"adb".into(),root:Some(uid.trim()=="0"),
            trace_available:Some(available.trim()=="AVAILABLE"),measurement_available:Some(available.trim()=="AVAILABLE"),control_verified:None})
    }
    pub fn deploy(&self)->Result<(),String> {
        let id=self.base.eval_identity()?;
        let remote=self.remote(); let trace=self.trace();
        if self.base.eval_present(&remote)? || self.base.eval_present(&trace)? {return Err("owned eval resource already exists; refusing overwrite".into());}
        self.update(|s|{s["identity"]=id.clone();s["deployment"]=json!("installing");})?;
        let owner=format!("{} {}",id["token"].as_str().unwrap(),id["boot_id"].as_str().ok_or("missing boot")?);
        self.mutate("eval_tools_directory",&["shell".into(),format!("set -eu; command -v setsid; test ! -e {d}; mkdir {d}; printf '%s\\n' {owner} > {d}/owner; sync",d=q(&remote),owner=q(&owner))],30_000,|s|s["tools_created"]=json!(true))?;
        for (name,text) in [("worker.sh",WORKER),("bootstrap.sh",BOOTSTRAP),("workload.sh",TRACE),("sample_env.sh",SAMPLER)] {
            let local=self.base.dir.join(name);new_file(&local,text.as_bytes())?;
            self.mutate("eval_push_tool",&["push".into(),local.to_string_lossy().into(),format!("{remote}/{name}")],30_000,|_|{})?;
        }
        self.update(|s|s["deployment"]=json!("ready"))?;
        self.write("eval_trace_create",&format!("{} && test ! -e {t} && mkdir {t}",self.owned()?,t=q(&trace)),|s|s["trace_state"]=json!("created"))?;
        if !self.base.eval_present(&trace)? {return Err("created trace instance absent".into());}
        Ok(())
    }
    fn worker(&self,mode:&str,run:&str,seconds:Option<u64>,mutation:bool)->Result<String,String> {
        let id=self.base.eval_identity()?;
        let extra=seconds.map(|s|format!(" {s} {}",q(&self.trace()))).unwrap_or_default();
        let cmd=format!("setsid sh {d}/worker.sh {d} {token} {boot} {mode} {run}{extra}",d=q(&self.remote()),
            token=q(id["token"].as_str().unwrap()),boot=q(id["boot_id"].as_str().ok_or("missing boot")?),mode=q(mode),run=q(run));
        let args=["shell".into(),format!("su -c {}",q(&cmd))];
        if mutation {self.mutate(mode,&args,(seconds.unwrap_or(0)+120)*1000,|_|{})}
        else {self.clear()?; Ok(self.base.eval_transport(mode,&args,30_000,false)?.stdout)}
    }
    pub fn sample(&self,label:&str,seconds:u64,out:&Path)->Result<(bool,bool),String> {
        if !(1..=86400).contains(&seconds) || label.is_empty() || !label.bytes().all(|b|b.is_ascii_alphanumeric()||b==b'_') {return Err("invalid finite gray sample".into());}
        fs::create_dir_all(out).map_err(|e|e.to_string())?;
        self.update(|s|s["rounds"].as_array_mut().unwrap().push(json!({"label":label,"seconds":seconds,"out":out})))?;
        let result=self.worker("gray_sample",label,Some(seconds),true)?;
        new_file(&out.join("capture.log"),result.as_bytes())?;
        if result.trim()!="GRAY_SAMPLE_COMPLETE" {return Err("gray sample lifecycle receipt missing".into());}
        let mut wc=None;let mut sc=None;
        // Retrieve failure evidence too; a missing trace is explicit after child status is saved.
        for (remote,local) in [("workload_exit.txt","workload_exit.txt"),("sampler_exit.txt","sampler_exit.txt"),("quiescence.txt","quiescence.txt"),("sampler.log","env.txt"),("workload.log","workload.log")] {
            let value=self.read("eval_sample_evidence",&format!("{} && cat {}/{label}/{remote}",self.owned()?,q(&self.remote())))?;
            new_file(&out.join(local),value.as_bytes())?;
            match remote {"workload_exit.txt"=>wc=Some(value.trim()=="0"),"sampler_exit.txt"=>sc=Some(value.trim()=="0"),"quiescence.txt" if value.trim()!="OWNED_GROUPS_ABSENT"=>return Err("sample groups not absent".into()),_=>{}}
        }
        if wc==Some(true) {
            let trace=self.read("eval_trace_evidence",&format!("{} && cat {}/{label}/trace.txt",self.owned()?,q(&self.remote())))?;
            new_file(&out.join("trace.txt"),trace.as_bytes())?;
        }
        Ok((wc==Some(true),sc==Some(true)))
    }
    fn identity(&self)->Result<(),String> {
        let id=self.base.eval_identity()?;
        let boot=id["boot_id"].as_str().ok_or("missing original boot")?;
        if self.read("eval_resource_identity","cat /proc/sys/kernel/random/boot_id")?.trim()!=boot {return Err("device boot changed; parent resource restoration refused".into());}
        Ok(())
    }
    pub fn fan_off(&self)->Result<Value,String> {
        if self.state.borrow()["deployment"]!="ready" {return Err("fan conditioning requires deployed eval resources".into());}
        self.identity()?;
        if !self.base.eval_present(FAN)? {return Ok(json!({"before":null,"after":null,"forced":false}));}
        let before=self.read("eval_fan_read",&format!("cat {FAN}"))?.trim().to_owned();
        if !matches!(before.as_str(),"0"|"1") {return Err("unrecognized fan value".into());}
        if self.state.borrow()["fan"].is_null() {self.update(|s|s["fan"]=json!({"original":before,"changed":false}))?;}
        if before!="0" {self.write("eval_fan_off",&format!("printf '0\\n' > {FAN}"),|s|s["fan"]["changed"]=json!(true))?;}
        let after=self.read("eval_fan_readback",&format!("cat {FAN}"))?.trim().to_owned();
        if after!="0" {return Err("fan off readback differs".into());}
        Ok(json!({"before":before,"after":after,"forced":before!="0"}))
    }
    pub fn wait_cool(&self,target:Option<f64>,timeout:u64)->Result<Value,String> {
        let start=Instant::now();
        loop {
            let raw=self.read("eval_temperature","for z in /sys/class/thermal/thermal_zone*; do [ -d \"$z\" ] || continue; t=$(cat \"$z/type\") || exit; v=$(cat \"$z/temp\") || exit; printf '%s %s\\n' \"$t\" \"$v\"; done")?;
            let best=raw.lines().filter_map(|line|{let mut p=line.split_whitespace();let z=p.next()?;let t=p.next()?.parse::<i64>().ok()?;
                ((z.starts_with("cpu-")||z.starts_with("cpullc")||z.starts_with("gpuss"))&&(0<t&&t<100000)).then(||(z.to_owned(),t))}).max_by_key(|(_,t)|*t);
            let Some((zone,t))=best else {return Ok(json!({"ok":false,"c":null,"timeout":false,"note":"读不到 SoC 热区"}));};
            let c=t as f64/1000.0; let elapsed=start.elapsed().as_secs();
            if target.is_none_or(|v|c<v) || elapsed>=timeout {return Ok(json!({"ok":target.is_some_and(|v|c<v),"zone":zone,"c":c,"waited_s":elapsed,"timeout":target.is_some_and(|v|c>=v)}));}
            std::thread::sleep(Duration::from_secs(8));
        }
    }
    pub fn admission_ready(&self)->Result<(),String> {
        self.clear()?;
        if self.worker("verify_gray_idle","verify",None,false)?.trim()!="GRAY_IDLE_QUIESCENT" {return Err("gray baseline children not currently quiescent".into());}
        if self.read("eval_trace_stopped",&format!("cat {}/tracing_on",q(&self.trace())))?.trim()!="0" {return Err("trace still active".into());}
        Ok(())
    }
    fn close_resources(&self)->Result<Value,String> {
        let state=self.state.borrow().clone();
        if state["deployment"]=="not_started" && state["trace_state"]=="not_started" && state["fan"].is_null() && state["pending"].is_null() {
            return Ok(json!({"no_parent_device_mutations":true,"remote_session_closed":null,"fan_restored":null}));
        }
        self.clear()?;self.identity()?;
        match state["deployment"].as_str() {
            Some("ready")=>{
                let closed=self.read("eval_closed",&format!("{} && if [ -d {}/session.closed ]; then echo CLOSED; else echo OPEN; fi",self.owned()?,q(&self.remote())))?;
                if closed.trim()=="OPEN" {
                    let run=format!("close{}_{}",std::process::id(),chrono::Utc::now().timestamp_micros());
                    if self.worker("reconcile_bench",&run,None,true)?.trim()!="BENCH_RECONCILE_COMPLETE" {return Err("eval child closure receipt missing".into());}
                } else if closed.trim()!="CLOSED" {return Err("unknown eval closure state".into());}
                if self.worker("verify_bench","verify",None,false)?.trim()!="BENCH_CLOSED_QUIESCENT" {return Err("eval remote lifecycle not quiescent".into());}
            },
            Some("installing") if state["rounds"].as_array().is_some_and(|a|a.is_empty()) => {
                // A known partial upload has never launched a worker. Close its
                // owned directory directly; the worker may not have arrived yet.
                if state["tools_created"]==true {
                    let closed=self.read("eval_partial_closed",&format!("{} && if [ -d {}/session.closed ]; then echo CLOSED; else echo OPEN; fi",self.owned()?,q(&self.remote())))?;
                    let id=self.base.eval_identity()?;
                    let owner=format!("{} {} partial",id["token"].as_str().unwrap(),id["boot_id"].as_str().unwrap());
                    if closed.trim()=="OPEN" {self.write("eval_partial_close",&format!("{} && mkdir {d}/session.closed && printf '%s\\n' {owner} > {d}/session.closed/owner && sync",self.owned()?,d=q(&self.remote()),owner=q(&owner)),|_|{})?;}
                    else if closed.trim()!="CLOSED" {return Err("unknown partial deployment closure".into());}
                    if self.read("eval_partial_owner",&format!("{} && cat {}/session.closed/owner",self.owned()?,q(&self.remote())))?.trim()!=owner {return Err("partial closure owner differs".into());}
                }
            },
            _=>return Err("invalid eval resource lifecycle".into()),
        }
        if state["trace_state"]=="created" {
            self.write("eval_trace_stop",&format!("{} && printf '0\\n' > {}/tracing_on",self.owned()?,q(&self.trace())),|_|{})?;
            if self.read("eval_trace_stop_readback",&format!("cat {}/tracing_on",q(&self.trace())))?.trim()!="0" {return Err("trace stop readback differs".into());}
            self.write("eval_trace_remove",&format!("{} && rmdir {}",self.owned()?,q(&self.trace())),|s|s["trace_state"]=json!("removed"))?;
        }
        if state["trace_state"]!="not_started" && self.base.eval_present(&self.trace())? {return Err("owned trace instance remains".into());}
        if state["fan"]["changed"]==true {
            let original=state["fan"]["original"].as_str().ok_or("missing original fan")?;
            self.write("eval_fan_restore",&format!("printf '%s\\n' {} > {FAN}",q(original)),|_|{})?;
            if self.read("eval_fan_restore_readback",&format!("cat {FAN}"))?.trim()!=original {return Err("fan restoration readback differs".into());}
        }
        Ok(json!({"remote_session_closed":state["tools_created"]==true,"owned_trace_absent":true,"fan_restored":true,
            "remote":self.remote(),"tool_policy":"retain closed ownership ledger, scripts and round evidence","charging_or_frequency_changed":false}))
    }
    pub fn finish(&self,error:Option<&str>)->Result<Value,String> {
        let resources=self.close_resources();
        // Never restore the layer while an unverified sampler/workload can still be running.
        let base=match &resources {Ok(_)=>self.base.restore_for_eval(error),Err(e)=>Err(e.clone())};
        let id=self.base.eval_identity()?;
        let path=self.base.dir.join(format!("eval-recovery-{}-{}.json",std::process::id(),chrono::Utc::now().timestamp_micros()));
        let verified=resources.is_ok() && base.as_ref().is_ok_and(|v|v["status"]=="verified");
        let mut value=json!({"schema":"phonefarm/gray-eval-launch-recovery@1","status":if verified{"verified"}else{"required"},
            "scope":"gray_eval_launch_owned_resources","serial":id["serial"],"token":id["token"],"session":self.base.dir,
            "evidence_origin":id["evidence_origin"],"base_recovery":id["base_recovery"],"base":base.as_ref().ok(),
            "resources":resources.as_ref().ok(),"error":resources.as_ref().err().or(base.as_ref().err()),
            "experiment_error":error,"game_state_restored":false,"report_path":path});
        new_file(&path,&serde_json::to_vec_pretty(&value).unwrap())?;
        if verified {self.base.complete_eval(&path)?;value["status"]=json!("reported_complete");}
        Ok(value)
    }
}
