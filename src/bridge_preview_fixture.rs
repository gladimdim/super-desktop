//! Disposable TLS bridge and HTTP service for emulator interoperability checks.
use super::super::*;
use std::io::BufRead;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Instant;

#[test]
#[ignore = "explicit emulator fixture; requires SD_PREVIEW_ANDROID_FIXTURE, never the live bridge"]
fn preview_android_fixture() {
    let file = PathBuf::from(
        std::env::var_os("SD_PREVIEW_ANDROID_FIXTURE").expect("fixture output path required"),
    );
    let token = super::super::tests::paired_token();
    let tls = security::tls_config().unwrap();
    let bridge = TcpListener::bind("127.0.0.1:0").unwrap();
    let service = TcpListener::bind("127.0.0.1:0").unwrap();
    bridge.set_nonblocking(true).unwrap();
    service.set_nonblocking(true).unwrap();
    let service_port = service.local_addr().unwrap().port();
    let document = serde_json::json!({"port":bridge.local_addr().unwrap().port(), "pin":security::fingerprint(),
        "token":token,"url":format!("http://localhost:{service_port}/")});
    use std::os::unix::fs::OpenOptionsExt;
    let mut out = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&file)
        .unwrap();
    out.write_all(document.to_string().as_bytes()).unwrap();
    let active = Arc::new(AtomicBool::new(true));
    let alive = active.clone();
    let service_thread = std::thread::spawn(move || {
        while alive.load(Ordering::Relaxed) {
            if let Ok((socket, _)) = service.accept() {
                std::thread::spawn(move || preview_service(socket));
            } else {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    });
    let until = Instant::now() + Duration::from_secs(300);
    while file.exists() && Instant::now() < until {
        if let Ok((socket, _)) = bridge.accept() {
            let tls = tls.clone();
            std::thread::spawn(move || {
                let admission = security::Admission::acquire(&socket);
                if admission.is_some() {
                    serve_connection(Connection::tls(socket, tls).unwrap(), admission);
                }
            });
        } else {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    active.store(false, Ordering::Relaxed);
    service_thread.join().unwrap();
    let _ = fs::remove_file(file);
}

fn preview_service(mut socket: TcpStream) {
    let _ = socket.set_read_timeout(Some(Duration::from_secs(15)));
    let mut reader = std::io::BufReader::new(&mut socket);
    let mut first = String::new();
    if reader.read_line(&mut first).is_err() {
        return;
    }
    let path = first.split_whitespace().nth(1).unwrap_or("").to_string();
    let mut headers = HashMap::new();
    loop {
        let mut line = String::new();
        if !matches!(reader.read_line(&mut line), Ok(n) if n > 0) {
            return;
        }
        if line == "\r\n" {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.to_lowercase(), value.trim().to_string());
        }
    }
    let count = headers
        .get("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0usize)
        .min(65536);
    let mut body = vec![0; count];
    if reader.read_exact(&mut body).is_err() {
        return;
    }
    drop(reader);
    if headers
        .get("cookie")
        .is_some_and(|v| v.contains("__sd_preview"))
        || headers.contains_key("authorization")
    {
        let _ = socket.write_all(
            b"HTTP/1.1 500 Credential leak\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        );
        return;
    }
    if path == "/ws" {
        if crate::ws::handshake(&mut socket, headers.get("sec-websocket-key").unwrap()).is_err() {
            return;
        }
        let mut ws = tungstenite::WebSocket::from_raw_socket(
            socket,
            tungstenite::protocol::Role::Server,
            None,
        );
        while let Ok(message) = ws.read() {
            if message.is_text() && ws.send(message).is_err() {
                break;
            }
        }
        return;
    }
    let (mime, bytes) = match path.as_str() {
        "/" => ("text/html", br#"<!doctype html><html><head><title>loading</title><link rel="stylesheet" href="/style.css"></head>
<body><div id="art">Private art preview</div><img id="pixel" src="/pixel.svg"><script>
let done=0;const pass=()=>{if(++done===6)document.title='PREVIEW_PASS'};
const fail=e=>{document.title='FAIL:'+e};document.cookie='theme=night; Path=/';
fetch('/api',{method:'POST',body:'hello'}).then(r=>r.text()).then(t=>t==='hello'?pass():fail(t)).catch(fail);
fetch('/redirect').then(r=>r.text()).then(t=>t==='redirected'?pass():fail(t)).catch(fail);
let ws=new WebSocket('ws://'+location.host+'/ws');ws.onopen=()=>ws.send('echo');ws.onmessage=e=>e.data==='echo'?pass():fail(e.data);ws.onerror=fail;
let es=new EventSource('/events');es.onmessage=e=>{if(e.data==='art'){pass();es.close()}else fail(e.data)};es.onerror=fail;
let img=document.getElementById('pixel');if(img.complete&&img.naturalWidth)pass();else {img.onload=pass;img.onerror=fail}
window.onload=()=>{if(getComputedStyle(document.getElementById('art')).color==='rgb(1, 2, 3)'&&!document.cookie.includes('__sd_preview'))pass();else fail('css/cookie')};
</script></body></html>"#.to_vec()),
        "/style.css" => ("text/css", b"#art { color: rgb(1, 2, 3); }".to_vec()),
        "/pixel.svg" => ("image/svg+xml", br#"<svg xmlns="http://www.w3.org/2000/svg" width="1" height="1"><rect width="1" height="1"/></svg>"#.to_vec()),
        "/api" => ("text/plain", body),
        "/events" => ("text/event-stream", b"data: art\n\n".to_vec()),
        "/redirect" => {
            let _ = socket.write_all(b"HTTP/1.1 302 Found\r\nLocation: /redirected\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            return;
        },
        "/redirected" => ("text/plain", b"redirected".to_vec()),
        _ => ("text/plain", b"not found".to_vec()),
    };
    let header = format!("HTTP/1.1 200 OK\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", bytes.len());
    let _ = socket
        .write_all(header.as_bytes())
        .and_then(|_| socket.write_all(&bytes));
}
