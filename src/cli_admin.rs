//! Read-only local administration and bounded snapshot streams.
use crate::cli::{Output,render_reply};
use crate::cli_extended::{Options,send,json_requested,opaque};
use crate::control::{self,Command,Reply,WorkspaceQuery};
use serde_json::{json,Value};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;

pub(crate) fn run(args:&[String])->Option<Output> {
    if !matches!(args.first()?.as_str(),"audit"|"access"|"doctor") {return None;}
    let build=||->Result<Output,&'static str>{
        let options=Options::parse(args,&["--after","--limit","--expect-revision","--output"],&[])?;
        if options.values.contains_key("--request-id"){return Err("Read-only administration does not accept request IDs.");}
        let words:Vec<_>=options.words.iter().map(String::as_str).collect();
        let reply=match words.as_slice(){
            ["access","list"] if options.values.keys().all(|k|matches!(k.as_str(),"--format"|"--target"))=>send(Command::Access {},&options,"access.list"),
            ["doctor"] if options.values.keys().all(|k|matches!(k.as_str(),"--format"|"--target"))=>{
                let status=send(Command::Status {},&options,"app.status");
                let capability=if status.ok {Some(send(Command::Capabilities {},&options,"capabilities"))}else{None};
                let mut reply=Reply::success("",json!({"clientVersion":env!("CARGO_PKG_VERSION"),"platform":std::env::consts::OS,"daemon":status,"capabilities":capability,"displayEnvironmentPresent":std::env::var_os("WAYLAND_DISPLAY").is_some()||std::env::var_os("DISPLAY").is_some(),"changesMade":false,"sessionsProbed":false}));
                if !status.ok {reply.ok=false;reply.error=status.error;}
                reply
            },
            ["audit",action @ ("list"|"export")]=>{
                let export=*action=="export";
                if export && options.values.keys().any(|k|matches!(k.as_str(),"--after"|"--limit"|"--expect-revision")){return Err("Export reads a complete stable metadata inventory; pagination options are for list.");}
                if !export && options.values.contains_key("--output"){return Err("--output is only for audit export.");}
                let mut after=options.values.get("--after").cloned();
                let mut revision=options.values.get("--expect-revision").cloned();
                let limit=options.values.get("--limit").map_or(Ok(100),|v|v.parse::<u16>().map_err(|_|"Use limit 1-100."))?;
                if !(1..=100).contains(&limit)||revision.as_ref().is_some_and(|v|!opaque(v)){return Err("Use limit 1-100 and a returned revision.");}
                let mut file=if export{Some(std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).custom_flags(libc::O_NOFOLLOW|libc::O_CLOEXEC).open(options.required("--output")?).map_err(|_|"Cannot create export file; destination must not exist.")?)}else{None};
                let mut count=0;
                loop {
                    let reply=send(Command::Audit {after:after.clone(),limit,expect_revision:revision.clone()},&options,"audit.list");
                    if !export||!reply.ok {break reply;}
                    let data=reply.data.as_ref().ok_or("Missing audit data.")?;
                    let entries=data["entries"].as_array().ok_or("Invalid audit entries.")?;
                    for entry in entries {serde_json::to_writer(file.as_mut().unwrap(),entry).map_err(|_|"Export write failed; partial file retained.")?;file.as_mut().unwrap().write_all(b"\n").map_err(|_|"Export write failed; partial file retained.")?;count+=1;}
                    if count>4096 {return Err("Audit export exceeded its bound; partial file retained.");}
                    let next=data["nextCursor"].as_str().map(str::to_owned);
                    if next.is_none(){file.as_mut().unwrap().sync_all().map_err(|_|"Export sync failed; partial file retained.")?;break Reply::success("",json!({"entries":count,"output":options.required("--output")?,"contentIncluded":false,"revision":data["revision"]}));}
                    if entries.is_empty()||next<=after {return Err("Invalid audit cursor; partial file retained.");}
                    revision=Some(data["revision"].as_str().ok_or("Missing audit revision.")?.into());after=next;
                }
            },
            _=>return Err("Unknown administration command or unsupported option. Read --help."),
        };
        Ok(render_reply(reply,options.json))
    };
    Some(build().unwrap_or_else(|m|render_reply(Reply::failure("","invalid_arguments",m),json_requested(args))))
}

pub(crate) fn events(args:&[String])->Option<i32>{
    if args.first()?.as_str()!="events"{return None;}
    fn emit(value:&Value)->Result<usize,()>{let output=render_reply_value(value);let mut out=std::io::stdout().lock();out.write_all(output.as_bytes()).and_then(|_|out.flush()).map_err(|_|())?;Ok(output.len())}
    fn render_reply_value(v:&Value)->String{serde_json::to_string(v).unwrap().chars().map(|c|if c.is_control(){format!("\\u{:04x}",c as u32)}else{c.to_string()}).collect::<String>()+"\n"}
    let run=||->Result<i32,(&'static str,&'static str)>{
        let mut normalized=args.to_vec();
        for i in 0..normalized.len(){if normalized[i]=="--format=jsonl"{normalized[i]="--format=json".into();}else if normalized[i]=="jsonl"&&i>0&&normalized[i-1]=="--format"{normalized[i]="json".into();}}
        if args.iter().any(|a|a.starts_with("--format=")&&a!="--format=jsonl")||args.windows(2).any(|p|p[0]=="--format"&&p[1]!="jsonl"){return Err(("invalid_arguments","Events emit JSONL."));}
        let opts=Options::parse(&normalized,&["--resource","--seconds","--interval-ms","--after"],&[]).map_err(|m|("invalid_arguments",m))?;
        if opts.words.len()!=1||opts.values.contains_key("--request-id"){return Err(("invalid_arguments","Events take no positional arguments or request ID."));}
        let resource=opts.required("--resource").map_err(|m|("invalid_arguments",m))?;
        let (command,method)=match resource.as_str(){"app"=>(Command::Status {},"app.status"),"terminals"=>(Command::Terminals {},"terminal.list"),"workspace"=>(Command::Workspace {query:WorkspaceQuery::Inspect},"workspace.read"),"notes"=>(Command::Workspace {query:WorkspaceQuery::Notes},"workspace.read"),_=>return Err(("invalid_arguments","Use resource app, terminals, workspace or notes."))};
        let duration=opts.values.get("--seconds").map_or(Ok(std::time::Duration::from_secs(10)),|v|crate::cli_extended::seconds(v)).map_err(|m|("invalid_arguments",m))?;
        let interval=opts.values.get("--interval-ms").map_or(Some(500),|v|v.parse::<u64>().ok()).filter(|n|(200..=10000).contains(n)).ok_or(("invalid_arguments","Use interval-ms 200-10000."))?;
        if opts.values.get("--after").is_some_and(|v|!opaque(v)){return Err(("invalid_arguments","Use a returned cursor."));}
        let stream=control::new_request(Command::Status {}).map_err(|_|("unavailable","Randomness unavailable."))?.request_id;
        let deadline=std::time::Instant::now()+duration;let mut sequence=0;let mut bytes=0;let mut previous=None;
        loop{
            let reply=send(command.clone(),&opts,method);
            if !reply.ok {emit(&serde_json::to_value(&reply).unwrap()).map_err(|_|("unavailable","Output closed."))?;return Ok(reply.exit_code());}
            let data=reply.data.ok_or(("invalid_response","Missing snapshot."))?;
            use sha2::{Digest,Sha256};let cursor=format!("{:x}",Sha256::digest(serde_json::to_vec(&(&resource,&data)).unwrap()));
            if previous.as_ref()!=Some(&cursor){
                bytes+=emit(&json!({"schemaVersion":1,"ok":true,"target":"local","type":"snapshot","streamId":stream,"sequence":sequence,"resource":resource,"cursor":cursor,"baseline":sequence==0,"resyncRequired":sequence==0&&opts.values.contains_key("--after"),"mayHaveGaps":true,"data":data})).map_err(|_|("unavailable","Output closed."))?;
                sequence+=1;previous=Some(cursor);
            }
            let now=std::time::Instant::now();let limited=sequence>=4096||bytes>=4*1024*1024;
            if now>=deadline||limited{emit(&json!({"schemaVersion":1,"ok":true,"target":"local","type":"end","streamId":stream,"sequence":sequence,"cursor":previous,"reason":if limited{"limit"}else{"duration"},"mayHaveGaps":true})).map_err(|_|("unavailable","Output closed."))?;return Ok(0);}
            std::thread::sleep(std::time::Duration::from_millis(interval).min(deadline-now));
        }
    };
    Some(match run(){Ok(code)=>code,Err((code,message))=>{let reply=Reply::failure("",code,message);if emit(&serde_json::to_value(&reply).unwrap()).is_err(){8}else{reply.exit_code()}}})
}
