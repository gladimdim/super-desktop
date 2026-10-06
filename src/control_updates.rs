//! Explicit update jobs; checking never installs, installation binds a reviewed commit.
use crate::control::{Command,Request,Reply};
use crate::updates::Status;
use serde_json::{json,Value};
use std::collections::BTreeMap;
use std::sync::{Arc,Mutex,OnceLock};
struct Job {state:&'static str,data:Value,checked:Option<(Status,String)>,consumed:bool}
static JOBS:OnceLock<Mutex<BTreeMap<String,Arc<Mutex<Job>>>>>=OnceLock::new();
fn jobs()->&'static Mutex<BTreeMap<String,Arc<Mutex<Job>>>>{JOBS.get_or_init(Mutex::default)}
fn describe(id:&str,job:&Job)->Value{json!({"jobId":id,"state":job.state,"data":job.data,"durable":false,"scope":"daemon-lifetime","installationConfirmed":false})}
fn public(status:&Status,commit:&str)->Value{json!({"currentVersion":status.current.to_string(),"latestVersion":status.latest.to_string(),"commit":commit,"available":status.available(),"blocked":status.blocked,"pinned":status.pinned,"changes":status.changes,"more":status.more,"installSupported":status.pinned.is_none()&&status.blocked.is_none()&&status.available()})}
pub fn execute(root:&std::path::Path,request:&Request)->Reply{
    execute_with(root,request,||{let status=crate::updates::check()?;let commit=crate::updates::cli_commit(&status)?;Ok((status,commit))},|status,commit|{
        let paths=crate::updates::Paths::user()?;let child=crate::updates::start_cli_update(&status,&commit,&paths)?;
        crate::updates::wait_rebuild(child,&paths)
    })
}
fn execute_with(root:&std::path::Path,request:&Request,check:impl FnOnce()->Result<(Status,String),String>+Send+'static,install:impl FnOnce(Status,String)->Result<(),String>+Send+'static)->Reply{
    let fail=|code,message|Reply::failure(&request.request_id,code,message);
    if let Command::UpdatesStatus {id}=&request.command{
        let job=jobs().lock().unwrap().get(id).cloned();
        return match job{Some(j)=>Reply::success(&request.request_id,describe(id,&j.lock().unwrap())),None=>fail("not_found","No update job has this ID in this daemon; inspect the durable request and running version.")};
    }
    crate::control_journal::execute(root,request,|_|{
        let mut all=jobs().lock().unwrap();
        if all.len()>=32{return fail("limit_reached","Update job inventory is full for this daemon lifetime.");}
        if all.values().any(|j|matches!(j.lock().unwrap().state,"checking"|"installing")){return fail("busy","An update job is already running.");}
        let selected=match &request.command{
            Command::UpdatesCheck {}=>None,
            Command::UpdatesInstall {check_id,expect_version,expect_commit,allow_install}=>{
                if !allow_install{return fail("denied","Installation requires --allow-install.");}
                let Some(previous)=all.get(check_id)else{return fail("not_found","Run updates check and use its completed job ID.");};
                let mut previous=previous.lock().unwrap();
                let Some((status,commit))=previous.checked.clone()else{return fail("invalid_state","The check did not finish successfully.");};
                if previous.consumed||expect_version!=&status.latest.to_string()||expect_commit!=&commit{return fail("conflict","Check is consumed or expected version/commit does not match.");}
                if !status.available()||status.blocked.is_some()||status.pinned.is_some(){return fail("invalid_state","The checked install is blocked, pinned or has no newer release.");}
                previous.consumed=true;Some((status,commit))
            },_=>return fail("invalid_request","Expected an update operation."),
        };
        let receipt=selected.as_ref().map(|(s,c)|public(s,c));
        let job=Arc::new(Mutex::new(Job {state:if selected.is_some(){"installing"}else{"checking"},data:receipt.clone().unwrap_or(json!({})),checked:None,consumed:false}));
        all.insert(request.request_id.clone(),job.clone());drop(all);
        let worker=job.clone();
        let spawned=std::thread::Builder::new().name("sd-cli-update".into()).spawn(move||{
            if let Some((status,commit))=selected{
                let result=install(status,commit);let mut j=worker.lock().unwrap();j.state=if result.is_ok(){"installer_exited"}else{"failed"};j.data["outcome"]=json!(if result.is_ok(){"installer_exited_check_running_version"}else{"installation_failed_inspect_update_log"});
            }else{
                let result=check();let mut j=worker.lock().unwrap();match result{Ok((status,commit))=>{j.data=public(&status,&commit);j.checked=Some((status,commit));j.state="checked";},Err(_)=>{j.state="failed";j.data=json!({"error":"Update check failed. Verify source-install setup, repository access and connectivity."});}}
            }
        });
        if spawned.is_err(){job.lock().unwrap().state="failed";return fail("operation_failed","Could not start update worker.");}
        Reply::success(&request.request_id,json!({"jobId":request.request_id,"outcome":"queued","operation":if receipt.is_some(){"install"}else{"check"},"reviewed":receipt,"jobStateDurable":false,"installationConfirmed":false}))
    })
}
#[cfg(test)]mod tests{
    use super::*;
    #[test]fn update_jobs_require_reviewed_identity_and_never_replay_installs(){
        let root=std::env::temp_dir().join(format!("sd-update-job-{}",std::process::id()));
        let request=|id:&str,command|Request {control_version:1,request_id:id.into(),command};
        let status=Status {current:crate::updates::Version(1,0,0),latest:crate::updates::Version(1,0,1),upstream:"origin/master".into(),url:"private".into(),dir:root.clone(),changes:vec![],more:0,blocked:None,pinned:None};
        let check=request("job-check",Command::UpdatesCheck {});
        assert!(execute_with(&root,&check,move||Ok((status,"a".repeat(40))),|_,_|panic!("check cannot install")).ok);
        let until=std::time::Instant::now()+std::time::Duration::from_secs(2);
        while jobs().lock().unwrap()["job-check"].lock().unwrap().state=="checking" {assert!(std::time::Instant::now()<until);std::thread::sleep(std::time::Duration::from_millis(5));}
        let command=|commit:String,allow_install|Command::UpdatesInstall {check_id:"job-check".into(),expect_version:"1.0.1".into(),expect_commit:commit,allow_install};
        let deny=request("deny",command("a".repeat(40),false));assert!(!execute_with(&root,&deny,||panic!(),|_,_|panic!()).ok);
        let stale=request("stale",command("b".repeat(40),true));assert_eq!(execute_with(&root,&stale,||panic!(),|_,_|panic!()).exit_code(),5);
        let install=request("install",command("a".repeat(40),true));let count=Arc::new(std::sync::atomic::AtomicUsize::new(0));let seen=count.clone();
        assert!(execute_with(&root,&install,||panic!(),move|_,commit|{assert_eq!(commit,"a".repeat(40));seen.fetch_add(1,std::sync::atomic::Ordering::SeqCst);Ok(())}).ok);
        assert!(execute_with(&root,&install,||panic!(),|_,_|panic!("do not reinstall")).ok);
        while count.load(std::sync::atomic::Ordering::SeqCst)==0{assert!(std::time::Instant::now()<until);std::thread::sleep(std::time::Duration::from_millis(5));}
        let _=std::fs::remove_dir_all(root);
    }
}
