//! Explicit local card removal and replacement, preserving receipt boundaries.
use crate::control::{Command,Reply,Request};
use crate::control_close::{Action,UiResult,Target};
use crate::control_launch::Prepared;
use serde_json::json;
use std::time::Instant;
fn prepare(card:&crate::state::TerminalData,native:Option<&str>)->Result<Prepared,(&'static str,&'static str)>{
    let cwd=card.workspace_dir.as_ref().ok_or(("invalid_state","The card has no recorded workspace."))?;
    let path=std::path::Path::new(cwd);
    if !path.is_absolute()||!path.is_dir()||cwd.contains('\0'){return Err(("invalid_state","The recorded workspace is unavailable."));}
    if card.command.is_empty()||card.command.len()>32768||card.command.contains('\0'){return Err(("invalid_state","The recorded command is invalid."));}
    let mut command=card.command.clone();
    if let Some(native)=native{
        let mut words=shlex::split(&command).ok_or(("unsupported_resume","Recorded launcher is not a direct command."))?;
        if words.first().and_then(|s|std::path::Path::new(s).file_name()).and_then(|s|s.to_str())!=Some(card.agent_type.as_str()){return Err(("unsupported_resume","Resume requires a direct Claude, Codex or OpenCode executable."));}
        let valid_uuid=native.len()==36&&native.bytes().enumerate().all(|(i,b)|if [8,13,18,23].contains(&i){b==b'-'}else{b.is_ascii_hexdigit()});
        let bad=match card.agent_type.as_str(){
            "claude"=>{if !valid_uuid{return Err(("invalid_arguments","Claude resume requires an exact session UUID."));}vec!["--resume","--continue","--fork-session","-r","-c"]},
            "codex"=>{if !valid_uuid{return Err(("invalid_arguments","Codex resume requires an exact thread UUID."));}vec!["resume","fork","exec","--last"]},
            "opencode"=>{if !native.starts_with("ses_")||native.len()>128||!native.bytes().all(|b|b.is_ascii_alphanumeric()||b==b'_'){return Err(("invalid_arguments","OpenCode requires an exact ses_ session ID."));}vec!["--session","--continue","--fork","-s","-c"]},
            _=>return Err(("unsupported_resume","Native resume is unsupported for this harness.")),
        };
        if words.iter().skip(1).any(|w|bad.contains(&w.split('=').next().unwrap())){return Err(("unsupported_resume","The saved command already selects a resume mode; launch explicitly instead."));}
        words.extend(match card.agent_type.as_str(){"claude"=>vec!["--resume".into(),native.into()],"codex"=>vec!["resume".into(),native.into()],_=>vec!["--session".into(),native.into()]});
        command=words.iter().map(|w|crate::launch_args::quote(w)).collect::<Vec<_>>().join(" ");
    }
    Ok(Prepared {harness:card.agent_type.clone(),directory:cwd.clone(),command})
}
pub fn execute(root:&std::path::Path,request:&Request,deadline:Instant,mut ui:impl FnMut(Action)->Result<UiResult,()>,adopt:impl FnOnce(crate::state::TerminalData,Instant)->Result<(),()>)->Reply{
    crate::control_journal::execute(root,request,|new_id|{
        let fail=|code,message|Reply::failure(&request.request_id,code,message);
        let target=match ui(Action::Inspect){Ok(Ok(Some(t)))=>t,Ok(Err(r))=>return r,_=>return fail("timeout","Card lookup expired before mutation.")};
        if matches!(request.command,Command::Forget {..}){
            return target.task.with_idle_until(deadline,||{
                match ui(Action::Remove(Target {data:target.data.clone(),task:target.task.clone()})){Ok(Ok(None))=>{},Ok(Err(r))=>return r,_=>return Reply::unknown(&request.request_id)};
                if crate::state::flush_state_saves_checked().is_err(){return Reply::unknown(&request.request_id);}
                Reply::success(&request.request_id,json!({"id":target.data.id,"sessionName":target.data.session_name,"outcome":"card_removed","sessionClosed":false,"runtimeObserved":false}))
            }).unwrap_or_else(||fail("busy","Terminal preparation prevented removal."));
        }
        let Command::Relaunch {id,native_session,allow_unsafe_harness,expect_epoch,expect_revision,expect_pane_identity}=&request.command else{unreachable!()};
        if !allow_unsafe_harness{return fail("unsafe_harness","Repeating a saved command requires --allow-unsafe-harness; it may execute commands or download software.");}
        let prepared=match prepare(&target.data,native_session.as_deref()){Ok(p)=>p,Err((c,m))=>return fail(c,m)};
        let close=Request {control_version:request.control_version,request_id:request.request_id.clone(),command:Command::Close {id:id.clone(),expect_epoch:expect_epoch.clone(),expect_revision:expect_revision.clone(),expect_pane_identity:expect_pane_identity.clone()}};
        let closed=crate::control_close::apply(&close,deadline,|action|{
            let result=ui(action)?;
            if let Ok(Some(current))=&result {if current.data.command!=target.data.command||current.data.workspace_dir!=target.data.workspace_dir||current.data.created_at!=target.data.created_at||!std::sync::Arc::ptr_eq(&current.task,&target.task){return Ok(Err(fail("conflict","The saved launcher changed before replacement.")));}}
            Ok(result)
        });
        if !closed.ok{return closed;}
        let partial=||{let mut reply=Reply::unknown(&request.request_id);reply.data=Some(json!({"replacedId":id,"reservedId":new_id,"oldSessionClosed":true,"newSessionObserved":false}));reply};
        let data=match crate::control_launch::start(&prepared,new_id,deadline){Ok(d)=>d,Err(_)=>return partial()};
        if adopt(data,deadline).is_err()||crate::state::flush_state_saves_checked().is_err(){return partial();}
        Reply::success(&request.request_id,json!({"id":new_id,"sessionName":new_id,"replacedId":id,"oldSessionClosed":true,"nativeSessionRequested":native_session,"nativeResumeObserved":false,"readiness":"not_observed","launchDirectory":prepared.directory,"outcome":"replaced"}))
    })
}
#[cfg(test)]mod tests{
    use super::*;
    #[test]fn resume_requires_explicit_native_identity_and_direct_launcher(){
        let mut card:crate::state::TerminalData=serde_json::from_value(json!({"id":"x","session_name":"x","agent_type":"codex","command":"/bin/codex --model 'literal ; text'","x":0,"y":0,"workspace_dir":"/","created_at":1})).unwrap();
        let id="12345678-1234-1234-1234-123456789abc";
        assert_eq!(shlex::split(&prepare(&card,Some(id)).unwrap().command).unwrap(),["/bin/codex","--model","literal ; text","resume",id]);
        assert!(prepare(&card,Some("--last")).is_err());card.command="/bin/sh -c codex".into();assert!(prepare(&card,Some(id)).is_err());
        card.command="codex resume --last".into();assert!(prepare(&card,Some(id)).is_err());
        card.agent_type="shell".into();card.command="/bin/sh".into();assert!(prepare(&card,Some(id)).is_err());assert!(prepare(&card,None).is_ok());
    }
}
