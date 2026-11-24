//! PC-side speech-to-text for recorded phone voice clips (`GET`/`POST /api/v1/transcribe`,
//! and inline voice transcription on `POST /api/v1/harnesses/<id>/attachment-prompt`).
//! Shells out cleanly without modifying user state; missing Whisper binaries fail closed.
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::Deserialize;
use std::borrow::Cow;
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub const ROUTE: &str = "/api/v1/transcribe";
/// Maximum decoded audio upload (8 MiB — more than 4 minutes of 16 kHz mono WAV).
pub const MAX_AUDIO_BYTES: usize = 8 * 1024 * 1024;
/// Base64 JSON upload limit for `POST /api/v1/transcribe`.
pub const MAX_BODY: usize = MAX_AUDIO_BYTES / 3 * 4 + 4 + 4096;
pub const UPLOAD_DEADLINE: Duration = Duration::from_secs(60);
const COMMAND_TIMEOUT: Duration = Duration::from_secs(45);
const MAX_OUTPUT_BYTES: u64 = 64 * 1024;

const MODEL_NAMES: &[&str] = &[
    "ggml-base.en.bin",
    "ggml-base.bin",
    "ggml-small.en.bin",
    "ggml-small.bin",
    "ggml-tiny.en.bin",
    "ggml-tiny.bin",
];

pub fn route(path: &str) -> bool {
    path == ROUTE
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SttBackend {
    WhisperCli { bin: PathBuf, model: PathBuf },
    WhisperCtranslate2 { bin: PathBuf },
    OpenAiWhisper { bin: PathBuf },
}

impl SttBackend {
    pub fn engine_name(&self) -> &'static str {
        match self {
            Self::WhisperCli { .. } => "whisper-cpp",
            Self::WhisperCtranslate2 { .. } => "whisper-ctranslate2",
            Self::OpenAiWhisper { .. } => "whisper",
        }
    }
}

pub fn looks_like_wav(bytes: &[u8]) -> bool {
    bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WAVE"
}

fn find_whisper_model(env_model: Option<&Path>, home: Option<&Path>) -> Option<PathBuf> {
    if let Some(path) = env_model.filter(|p| p.is_file()) {
        return Some(path.to_path_buf());
    }
    let mut dirs = Vec::new();
    if let Some(home) = home {
        dirs.push(home.join(".local/share/whisper"));
        dirs.push(home.join(".cache/whisper"));
    }
    dirs.push(PathBuf::from("/usr/share/whisper.cpp"));
    dirs.push(PathBuf::from("/usr/share/whisper"));
    dirs.push(PathBuf::from("/usr/local/share/whisper"));
    for dir in &dirs {
        for name in MODEL_NAMES {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    for dir in &dirs {
        let Ok(entries) = fs::read_dir(dir) else { continue };
        let mut bins: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.is_file()
                    && p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with("ggml-") && n.ends_with(".bin"))
            })
            .collect();
        bins.sort();
        if let Some(first) = bins.into_iter().next() {
            return Some(first);
        }
    }
    None
}

pub fn detect_backend_with(
    which: &dyn Fn(&str) -> Option<PathBuf>,
    env_model: Option<&Path>,
    home: Option<&Path>,
) -> Option<SttBackend> {
    if let Some(bin) = which("whisper-cli").or_else(|| which("whisper-cpp")) {
        if let Some(model) = find_whisper_model(env_model, home) {
            return Some(SttBackend::WhisperCli { bin, model });
        }
    }
    if let Some(bin) = which("whisper-ctranslate2") {
        return Some(SttBackend::WhisperCtranslate2 { bin });
    }
    if let Some(bin) = which("whisper") {
        return Some(SttBackend::OpenAiWhisper { bin });
    }
    None
}

pub fn detect_backend() -> Option<SttBackend> {
    let env_model = std::env::var_os("SD_WHISPER_MODEL").map(PathBuf::from);
    let home = std::env::var_os("HOME").map(PathBuf::from);
    detect_backend_with(&|name| crate::tmux::which(name).map(PathBuf::from), env_model.as_deref(), home.as_deref())
}

pub fn status_json() -> serde_json::Value {
    match detect_backend() {
        Some(backend) => serde_json::json!({
            "available": true,
            "engine": backend.engine_name(),
            "maxAudioBytes": MAX_AUDIO_BYTES,
        }),
        None => serde_json::json!({
            "available": false,
            "maxAudioBytes": MAX_AUDIO_BYTES,
        }),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Body<'a> {
    #[serde(default, borrow)]
    data_base64: Cow<'a, str>,
    #[serde(default, borrow)]
    mime_type: Option<Cow<'a, str>>,
}

pub fn parse_request(body: &str) -> Result<Vec<u8>, &'static str> {
    let parsed: Body = serde_json::from_str(body).map_err(|_| "bad_json")?;
    if let Some(mime) = parsed.mime_type.as_deref() {
        let lower = mime.trim().to_ascii_lowercase();
        if !matches!(lower.as_str(), "audio/wav" | "audio/x-wav" | "audio/wave" | "audio/l16") {
            return Err("unsupported_mime_type");
        }
    }
    if parsed.data_base64.is_empty() {
        return Err("empty_audio");
    }
    if parsed.data_base64.len() / 4 * 3 > MAX_AUDIO_BYTES + 4 {
        return Err("audio_too_large");
    }
    let bytes = STANDARD
        .decode(parsed.data_base64.as_ref())
        .map_err(|_| "invalid_audio_encoding")?;
    if bytes.is_empty() {
        return Err("empty_audio");
    }
    if bytes.len() > MAX_AUDIO_BYTES {
        return Err("audio_too_large");
    }
    if !looks_like_wav(&bytes) {
        return Err("invalid_wav");
    }
    Ok(bytes)
}

struct TempDir(PathBuf);

impl TempDir {
    fn new(prefix: &str) -> Result<Self, &'static str> {
        let base = std::env::var_os("XDG_RUNTIME_DIR")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .map(|p| p.join("super-desktop/transcribe"))
            .or_else(|| {
                std::env::var_os("HOME")
                    .filter(|v| !v.is_empty())
                    .map(PathBuf::from)
                    .map(|p| p.join(".local/state/super-desktop/transcribe"))
            })
            .unwrap_or_else(|| {
                std::env::temp_dir().join(format!("sd-transcribe-u{}", unsafe { libc::geteuid() }))
            });
        crate::control::ensure_private_dir(&base).map_err(|_| "transcription_failed")?;
        for _ in 0..8 {
            let nonce = crate::control::random_hex(16).map_err(|_| "transcription_failed")?;
            let dir = base.join(format!("{prefix}-{nonce}"));
            match DirBuilder::new().mode(0o700).create(&dir) {
                Ok(()) => return Ok(Self(dir)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(_) => return Err("transcription_failed"),
            }
        }
        Err("transcription_failed")
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn clean_transcript(raw: &str) -> String {
    raw.lines()
        .map(|line| {
            let trimmed = line.trim();
            if let Some(rest) = trimmed.strip_prefix('[') {
                if let Some((_, after)) = rest.split_once("-->") {
                    if let Some((_, text)) = after.split_once(']') {
                        return text.trim();
                    }
                }
            }
            trimmed
        })
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn transcribe_wav(wav: &[u8]) -> Result<(String, &'static str), &'static str> {
    let backend = detect_backend().ok_or("stt_unavailable")?;
    transcribe_wav_with(&backend, wav, COMMAND_TIMEOUT)
}

pub fn transcribe_wav_with(
    backend: &SttBackend,
    wav: &[u8],
    timeout: Duration,
) -> Result<(String, &'static str), &'static str> {
    if !looks_like_wav(wav) || wav.len() > MAX_AUDIO_BYTES {
        return Err("invalid_wav");
    }
    let temp = TempDir::new("sd-transcribe")?;
    let wav_path = temp.path().join("clip.wav");
    {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&wav_path)
            .map_err(|_| "transcription_failed")?;
        file.write_all(wav).map_err(|_| "transcription_failed")?;
    }

    let mut cmd = match backend {
        SttBackend::WhisperCli { bin, model } => {
            let mut c = Command::new(bin);
            c.args(["-m"]).arg(model).args(["-f"]).arg(&wav_path).args(["-nt", "-np"]);
            c
        }
        SttBackend::WhisperCtranslate2 { bin } | SttBackend::OpenAiWhisper { bin } => {
            let mut c = Command::new(bin);
            c.arg(&wav_path)
                .args(["--output_format", "txt", "--output_dir"])
                .arg(temp.path());
            c
        }
    };
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0);

    let mut child = cmd.spawn().map_err(|_| "transcription_failed")?;
    let pgid = child.id() as libc::pid_t;
    let stdout_pipe = child.stdout.take();
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(out) = stdout_pipe {
            let _ = out.take(MAX_OUTPUT_BYTES).read_to_end(&mut buf);
        }
        buf
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() >= timeout => {
                unsafe { libc::kill(-pgid, libc::SIGKILL) };
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                return Err("transcription_timeout");
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(15)),
            Err(_) => {
                unsafe { libc::kill(-pgid, libc::SIGKILL) };
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                return Err("transcription_failed");
            }
        }
    };
    let stdout_bytes = reader.join().unwrap_or_default();
    if !status.success() {
        return Err("transcription_failed");
    }

    let raw = match backend {
        SttBackend::WhisperCli { .. } => String::from_utf8_lossy(&stdout_bytes).into_owned(),
        SttBackend::WhisperCtranslate2 { .. } | SttBackend::OpenAiWhisper { .. } => {
            let txt = temp.path().join("clip.txt");
            let from_file = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
                .open(&txt)
                .ok()
                .and_then(|f| {
                    let meta = f.metadata().ok()?;
                    if !meta.is_file()
                        || meta.uid() != unsafe { libc::geteuid() }
                        || meta.nlink() != 1
                    {
                        return None;
                    }
                    let mut buf = Vec::new();
                    f.take(MAX_OUTPUT_BYTES).read_to_end(&mut buf).ok()?;
                    Some(String::from_utf8_lossy(&buf).into_owned())
                });
            from_file.unwrap_or_else(|| String::from_utf8_lossy(&stdout_bytes).into_owned())
        }
    };
    Ok((clean_transcript(&raw), backend.engine_name()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_wav(extra: usize) -> Vec<u8> {
        let mut bytes = b"RIFF\x24\x00\x00\x00WAVEfmt \x10\x00\x00\x00\x01\x00\x01\x00\x80>\x00\x00\x00}\x00\x00\x02\x00\x10\x00data\x00\x00\x00\x00".to_vec();
        bytes.resize(bytes.len() + extra, 0);
        bytes
    }

    fn write_script(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        {
            let mut f = OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .mode(0o755)
                .open(&path)
                .unwrap();
            f.write_all(body.as_bytes()).unwrap();
        }
        path
    }

    #[test]
    fn parse_request_validates_json_wav_mime_and_size() {
        let ok = serde_json::json!({
            "dataBase64": STANDARD.encode(fake_wav(16)),
            "mimeType": "audio/wav"
        })
        .to_string();
        assert!(looks_like_wav(&parse_request(&ok).unwrap()));

        assert_eq!(parse_request("not json").unwrap_err(), "bad_json");
        assert_eq!(parse_request("{}").unwrap_err(), "empty_audio");
        assert_eq!(
            parse_request(&serde_json::json!({"dataBase64":"a","mimeType":"audio/ogg"}).to_string())
                .unwrap_err(),
            "unsupported_mime_type"
        );
        assert_eq!(
            parse_request(&serde_json::json!({"dataBase64": STANDARD.encode(b"not-a-wav-header")}).to_string())
                .unwrap_err(),
            "invalid_wav"
        );
    }

    #[test]
    fn detect_backend_prefers_whisper_cpp_with_model_then_fallbacks() {
        let tmp = TempDir::new("sd-stt-detect").unwrap();
        let home = tmp.path().join("home");
        let model_dir = home.join(".local/share/whisper");
        fs::create_dir_all(&model_dir).unwrap();
        let model = model_dir.join("ggml-base.en.bin");
        fs::write(&model, b"ggml").unwrap();

        let cpp = tmp.path().join("whisper-cli");
        let ct2 = tmp.path().join("whisper-ctranslate2");
        let py = tmp.path().join("whisper");

        // All present -> whisper-cpp selected.
        let picked = detect_backend_with(
            &|name| match name {
                "whisper-cli" => Some(cpp.clone()),
                "whisper-ctranslate2" => Some(ct2.clone()),
                "whisper" => Some(py.clone()),
                _ => None,
            },
            None,
            Some(&home),
        );
        assert_eq!(
            picked,
            Some(SttBackend::WhisperCli {
                bin: cpp.clone(),
                model: model.clone(),
            })
        );

        // No model -> skips whisper-cli and falls back to whisper-ctranslate2.
        let picked_no_model = detect_backend_with(
            &|name| match name {
                "whisper-cli" => Some(cpp.clone()),
                "whisper-ctranslate2" => Some(ct2.clone()),
                _ => None,
            },
            None,
            None,
        );
        assert_eq!(
            picked_no_model,
            Some(SttBackend::WhisperCtranslate2 { bin: ct2 })
        );

        // Explicit env model overrides home search.
        let custom = tmp.path().join("custom.bin");
        fs::write(&custom, b"ggml").unwrap();
        let picked_env = detect_backend_with(
            &|name| (name == "whisper-cpp").then(|| cpp.clone()),
            Some(&custom),
            Some(&home),
        );
        assert_eq!(
            picked_env,
            Some(SttBackend::WhisperCli {
                bin: cpp,
                model: custom,
            })
        );
    }

    #[test]
    fn transcribe_wav_runs_backends_strips_timestamps_and_times_out() {
        let tmp = TempDir::new("sd-stt-run").unwrap();
        let model = tmp.path().join("ggml-base.bin");
        fs::write(&model, b"m").unwrap();
        let cpp = write_script(
            tmp.path(),
            "whisper-cli",
            "#!/bin/sh\necho '[00:00.000 --> 00:02.000]  Fix the toolbar bug'\necho ' right now.'\n",
        );
        let (text, engine) = transcribe_wav_with(
            &SttBackend::WhisperCli { bin: cpp, model },
            &fake_wav(32),
            Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(text, "Fix the toolbar bug right now.");
        assert_eq!(engine, "whisper-cpp");

        let ct2 = write_script(
            tmp.path(),
            "whisper-ctranslate2",
            "#!/bin/sh\nout=''\nwhile [ \"$#\" -gt 0 ]; do\n  if [ \"$1\" = '--output_dir' ]; then out=\"$2\"; shift 2; else shift; fi\ndone\nprintf 'Ship it\\n' > \"$out/clip.txt\"\n",
        );
        let (text2, engine2) = transcribe_wav_with(
            &SttBackend::WhisperCtranslate2 { bin: ct2 },
            &fake_wav(32),
            Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(text2, "Ship it");
        assert_eq!(engine2, "whisper-ctranslate2");

        let fail = write_script(tmp.path(), "fail", "#!/bin/sh\nexit 1\n");
        assert_eq!(
            transcribe_wav_with(
                &SttBackend::OpenAiWhisper { bin: fail },
                &fake_wav(32),
                Duration::from_secs(5),
            )
            .unwrap_err(),
            "transcription_failed"
        );

        let slow = write_script(tmp.path(), "slow", "#!/bin/sh\nsleep 2\n");
        assert_eq!(
            transcribe_wav_with(
                &SttBackend::OpenAiWhisper { bin: slow },
                &fake_wav(32),
                Duration::from_millis(50),
            )
            .unwrap_err(),
            "transcription_timeout"
        );
    }
}
