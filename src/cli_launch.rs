//! One-shot launch arguments are explicit and never saved as launcher defaults.
use crate::cli::{Output,render_reply};
use crate::cli_extended::{Options,send,json_requested,valid_id};
use crate::control::{Command,Reply};
pub(crate) fn run(args:&[String])->Option<Output>{
    if !matches!(args.get(0..2).map(|a|(a[0].as_str(),a[1].as_str())),Some(("harness","launch")|("terminal","create"))){return None;}
    let build=||->Result<Output,&'static str>{
        let options=Options::parse(args,&["--cwd","--args-file"],&["--allow-unsafe-harness","--allow-download"])?;
        let harness=match options.words.iter().map(String::as_str).collect::<Vec<_>>().as_slice(){["harness","launch",id] if valid_id(id,64)=>(*id).to_owned(),["terminal","create"] if !options.flags.contains("--allow-download")=>"shell".into(),_=>return Err("Use harness launch ID or terminal create; read --help.")};
        let cwd=options.required("--cwd")?;if !std::path::Path::new(&cwd).is_absolute()||cwd.len()>4096||cwd.contains('\0'){return Err("Use an absolute working directory of at most 4096 bytes.");}
        let arguments=if let Some(path)=options.values.get("--args-file"){
            let input=Options::parse(&["--file".into(),path.clone()],&["--file"],&[])?;
            let arguments:Vec<String>=serde_json::from_str(&input.text(12000)?).map_err(|_|"Arguments must be a JSON array of strings.")?;
            if arguments.len()>32||arguments.iter().any(|s|s.len()>1024||s.chars().any(char::is_control)){return Err("Use at most 32 arguments of 1024 bytes each, without controls.");}
            if !options.flags.contains("--allow-unsafe-harness"){return Err("One-shot arguments require --allow-unsafe-harness.");}
            Some(arguments)
        }else{None};
        Ok(render_reply(send(Command::Launch {harness,cwd,arguments,allow_unsafe_harness:options.flags.contains("--allow-unsafe-harness"),allow_download:options.flags.contains("--allow-download")},&options,"harness.launch"),options.json))
    };
    Some(build().unwrap_or_else(|m|render_reply(Reply::failure("","invalid_arguments",m),json_requested(args))))
}
