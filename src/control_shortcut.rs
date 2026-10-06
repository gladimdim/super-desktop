//! Local preview/apply for the managed shortcut block. Never used by the bridge.
use crate::control::{Command,Reply,Request};
use serde_json::{json,Value};
use sha2::{Digest,Sha256};
use std::io::{Read,Write};
use std::os::unix::fs::{MetadataExt,OpenOptionsExt};
use std::path::Path;
use std::time::Instant;
pub struct Query {pub request:Request,pub commit:bool,pub responder:std::sync::mpsc::SyncSender<Reply>,pub deadline:Instant}
type Failure=(&'static str,&'static str);
pub fn canonical(combo:&str)->Result<String,Failure>{
    use gtk4::gdk;
    if combo.len()>128{return Err(("invalid_arguments","Shortcut is too long."));}
    let parts:Vec<_>=combo.split('+').map(str::trim).collect();
    let (key,mods)=parts.split_last().ok_or(("invalid_arguments","Missing shortcut."))?;
    if key.is_empty()||!key.bytes().all(|b|b.is_ascii_alphanumeric()||b==b'_'){return Err(("invalid_arguments","Use a named key and SUPER/CTRL/ALT/SHIFT modifiers."));}
    let mut mask=gdk::ModifierType::empty();
    for part in mods {let bit=match *part{"SUPER"=>gdk::ModifierType::SUPER_MASK,"CTRL"=>gdk::ModifierType::CONTROL_MASK,"ALT"=>gdk::ModifierType::ALT_MASK,"SHIFT"=>gdk::ModifierType::SHIFT_MASK,_=>return Err(("invalid_arguments","Unknown shortcut modifier."))};if mask.contains(bit){return Err(("invalid_arguments","Duplicate modifier."));}mask|=bit;}
    let key=gdk::Key::from_name(*key).ok_or(("invalid_arguments","Unknown key name."))?;
    match crate::shortcut::interpret(key,mask){crate::shortcut::Capture::Combo(combo)=>Ok(combo),_=>Err(("invalid_arguments","Use a real modifier plus key, or F1-F12 alone."))}
}
fn read(path:&Path)->Result<String,Failure>{
    let mut file=std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW|libc::O_NONBLOCK|libc::O_CLOEXEC).open(path).map_err(|_|("unavailable","Cannot read existing bindings.lua."))?;
    let m=file.metadata().map_err(|_|("unavailable","Cannot inspect bindings."))?;
    if !m.is_file()||m.uid()!=unsafe{libc::geteuid()}||m.nlink()!=1||m.mode()&0o022!=0||m.len()>131072{return Err(("denied","Bindings must be an owned regular file, unlinked, not writable by others, at most128KiB."));}
    let mut text=String::new();Read::by_ref(&mut file).take(131073).read_to_string(&mut text).map_err(|_|("unavailable","Bindings are not readable UTF-8."))?;
    if text.len()>131072{return Err(("unavailable","Bindings grew while reading."));}Ok(text)
}
fn hypr(bin:&str,args:&[&str],deadline:Instant)->Result<String,Failure>{
    let mut process=std::process::Command::new(bin);process.args(args);
    let output=crate::control_terminal::read_process(process,131072,deadline)?;
    if output.limited{return Err(("output_limit","Compositor output exceeded its limit."));}
    String::from_utf8(output.bytes).map_err(|_|("unavailable","Invalid compositor output."))
}
fn plan(path:&Path,bin:&str,combo:&str,deadline:Instant)->Result<Value,Failure>{
    let combo=canonical(combo)?;
    let before=read(path)?;let bindings=hypr(bin,&["binds"],deadline)?;
    let after=crate::shortcut::rewrite_bindings(&before,&combo,None);
    let conflict=crate::shortcut::conflicting_bind(&bindings,&combo);
    let token=format!("{:x}",Sha256::digest(serde_json::to_vec(&(path,&before,&after,&bindings)).unwrap()));
    Ok(json!({"combo":combo,"path":path,"before":before,"after":after,"conflict":conflict,"preview":token,"physicalKeycode":null,"changesMade":false}))
}
fn replace(path:&Path,text:&str,nonce:&str)->Result<(),Failure>{
    let temporary=path.with_file_name(format!(".super-desktop-shortcut-{nonce}"));
    let mut file=std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).custom_flags(libc::O_NOFOLLOW|libc::O_CLOEXEC).open(&temporary).map_err(|_|("operation_failed","Cannot create shortcut replacement."))?;
    let result=file.write_all(text.as_bytes()).and_then(|_|file.sync_all()).and_then(|_|std::fs::rename(&temporary,path)).and_then(|_|std::fs::File::open(path.parent().unwrap())?.sync_all());
    if result.is_err(){let _=std::fs::remove_file(temporary);return Err(("operation_failed","Shortcut replacement failed; inspect the backup and bindings."));}Ok(())
}
fn apply(path:&Path,bin:&str,planned:&Value,nonce:&str,deadline:Instant)->Result<Value,Failure>{
    let before=planned["before"].as_str().unwrap();let after=planned["after"].as_str().unwrap();
    if read(path)?!=before{return Err(("conflict","Bindings changed after preview."));}
    let backup=path.with_file_name(format!("bindings.lua.super-desktop-{nonce}.bak"));
    let mut file=std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).custom_flags(libc::O_NOFOLLOW|libc::O_CLOEXEC).open(&backup).map_err(|_|("operation_failed","Cannot create a new private bindings backup."))?;
    file.write_all(before.as_bytes()).and_then(|_|file.sync_all()).map_err(|_|("operation_failed","Cannot save bindings backup."))?;
    if Instant::now()>=deadline||read(path)?!=before{return Err(("conflict","Bindings changed or request expired before apply."));}
    replace(path,after,nonce)?;
    let validation=hypr(bin,&["reload"],deadline).and_then(|_|hypr(bin,&["configerrors"],deadline)).and_then(|s|if s.trim().is_empty()||s.trim().eq_ignore_ascii_case("ok"){Ok(())}else{Err(("configuration_error","Hyprland reported configuration errors."))});
    if validation.is_err(){
        // Only undo our own bytes; never overwrite an intervening user edit.
        let restored=read(path).is_ok_and(|s|s==after)&&replace(path,before,&format!("{nonce}-rollback")).is_ok();
        let rollback_live=restored&&hypr(bin,&["reload"],deadline).and_then(|_|hypr(bin,&["configerrors"],deadline)).is_ok_and(|s|s.trim().is_empty()||s.trim().eq_ignore_ascii_case("ok"));
        return Ok(json!({"outcome":"validation_failed","backup":backup,"rolledBack":restored,"rollbackValidated":rollback_live,"applied":false}));
    }
    Ok(json!({"outcome":"applied","backup":backup,"combo":planned["combo"],"conflict":planned["conflict"],"validated":true,"applied":true}))
}
pub fn execute(root:&Path,request:&Request,deadline:Instant,mut ui:impl FnMut(bool)->Result<Reply,()>)->Reply{
    let Command::Shortcut {combo,preview,..}=&request.command else{unreachable!()};
    let fail=|(code,message)|Reply::failure(&request.request_id,code,message);
    if !cfg!(target_os="linux"){return fail(("unsupported_platform","CLI shortcut preview/apply currently requires Linux Hyprland."));}
    let mut run=||{
        let state=match ui(false){Ok(r) if r.ok=>r,Ok(r)=>return r,Err(_)=>return fail(("timeout","Shortcut state lookup expired."))};
        let planned=match plan(&crate::shortcut::bindings_path(),"hyprctl",combo,deadline){Ok(p)=>p,Err(e)=>return fail(e)};
        let Some(token)=preview else{let mut data=state.data.unwrap();data["plan"]=planned;return Reply::success(&request.request_id,data);};
        if token!=planned["preview"].as_str().unwrap(){return fail(("conflict","Shortcut preview changed; preview again before applying."));}
        let result=match apply(&crate::shortcut::bindings_path(),"hyprctl",&planned,&request.request_id,deadline){Ok(r)=>r,Err(_)=>return Reply::unknown(&request.request_id)};
        if result["applied"]!=true{let mut reply=fail(("configuration_error","Shortcut validation failed; inspect rollback status and the backup."));reply.error.as_mut().unwrap().outcome=if result["rolledBack"]==true{"rolled_back"}else{"unknown"}.into();reply.data=Some(result);return reply;}
        match ui(true){Ok(r)if r.ok=>{},_=>return Reply::unknown(&request.request_id)};
        if crate::state::flush_state_saves_checked().is_err(){return Reply::unknown(&request.request_id);}
        Reply::success(&request.request_id,result)
    };
    if preview.is_some(){crate::control_journal::execute(root,request,|_|run())}else{run()}
}
#[cfg(test)] mod tests{
    use super::*;use std::os::unix::fs::PermissionsExt;
    #[test] fn shortcut_preview_binds_file_and_runtime_then_backs_up_and_validates(){
        let root=std::env::temp_dir().join(format!("sd-cli-shortcut-{}",std::process::id()));std::fs::create_dir_all(&root).unwrap();let path=root.join("bindings.lua");std::fs::write(&path,"-- existing custom binding\n").unwrap();
        let bin=root.join("hyprctl");std::fs::write(&bin,"#!/bin/sh\nprintf '%s\\n' \"$1\" >> \"$0.calls\"\ncase \"$1\" in binds) printf '';; configerrors) printf ok;; esac\n").unwrap();std::fs::set_permissions(&bin,std::fs::Permissions::from_mode(0o700)).unwrap();
        let deadline=||Instant::now()+std::time::Duration::from_secs(2);
        let p=plan(&path,bin.to_str().unwrap(),"SUPER + CTRL + F8",deadline()).unwrap();assert!(p["after"].as_str().unwrap().contains("hl.unbind"));
        std::fs::write(&path,"-- concurrent change\n").unwrap();assert!(apply(&path,bin.to_str().unwrap(),&p,"stale",deadline()).is_err());
        std::fs::write(&path,p["before"].as_str().unwrap()).unwrap();let result=apply(&path,bin.to_str().unwrap(),&p,"good",deadline()).unwrap();assert_eq!(result["validated"],true);assert_eq!(std::fs::read_to_string(result["backup"].as_str().unwrap()).unwrap(),p["before"]);
        assert_eq!(std::fs::metadata(result["backup"].as_str().unwrap()).unwrap().mode()&0o777,0o600);let calls=std::fs::read_to_string(bin.with_extension("calls")).unwrap();assert!(calls.ends_with("reload\nconfigerrors\n"));
        assert!(canonical("SUPER + Q\"; os.execute('bad')").is_err());assert!(canonical("Q").is_err());assert!(canonical("F8").is_ok());
        std::fs::write(&bin,"#!/bin/sh\ncase \"$1\" in configerrors) printf broken;; esac\n").unwrap();let p=plan(&path,bin.to_str().unwrap(),"SUPER + F9",deadline()).unwrap();let r=apply(&path,bin.to_str().unwrap(),&p,"rollback",deadline()).unwrap();assert_eq!(r["rolledBack"],true);assert_eq!(r["rollbackValidated"],false);assert_eq!(read(&path).unwrap(),p["before"]);
        let _=std::fs::remove_dir_all(root);
    }
}
