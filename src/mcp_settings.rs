//! Persistent local MCP tool preferences shared by the GUI and stdio clients.
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Config {
    pub enabled: bool,
    pub read_output: bool,
    pub launch: bool,
    pub prompts: bool,
}
impl Default for Config {
    fn default() -> Self {
        // Preserve the original metadata-only MCP interface for existing clients.
        Self {
            enabled: true,
            read_output: false,
            launch: false,
            prompts: false,
        }
    }
}
impl Config {
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            ..Self::default()
        }
    }
    pub fn allows(&self, tool: &str) -> bool {
        self.enabled
            && match tool {
                "capture_terminal" => self.read_output,
                "launch_harness" => self.launch,
                "submit_prompt" => self.prompts,
                _ => true,
            }
    }
}

pub fn config_root() -> io::Result<PathBuf> {
    let root = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .map(|p| p.join(".config"))
        })
        .ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "No user configuration directory")
        })?;
    Ok(root)
}
pub fn path() -> io::Result<PathBuf> {
    Ok(config_root()?.join("super-desktop/mcp.json"))
}
pub fn load() -> io::Result<Config> {
    load_at(&path()?)
}
pub fn save(config: Config) -> io::Result<()> {
    save_at(&path()?, config)
}

pub fn load_at(path: &Path) -> io::Result<Config> {
    if let Some(parent) = path.parent() {
        match fs::symlink_metadata(parent) {
            Ok(metadata)
                if !metadata.is_dir()
                    || metadata.uid() != unsafe { libc::getuid() }
                    || metadata.mode() & 0o022 != 0 =>
            {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "Unsafe MCP settings directory",
                ));
            }
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
    }
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
    {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Config::default()),
        Err(e) => return Err(e),
    };
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::getuid() }
        || metadata.mode() & 0o077 != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "MCP settings must be an owner-only regular file",
        ));
    }
    let mut bytes = vec![];
    file.take(16385).read_to_end(&mut bytes)?;
    if bytes.len() > 16384 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "MCP settings are too large",
        ));
    }
    serde_json::from_slice(&bytes)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "Invalid MCP settings"))
}

pub fn save_at(path: &Path, config: Config) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Invalid settings path"))?;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(parent)?;
    let metadata = fs::symlink_metadata(parent)?;
    if !metadata.is_dir()
        || metadata.uid() != unsafe { libc::getuid() }
        || metadata.mode() & 0o022 != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "MCP settings directory must be owned by this user and not writable by others",
        ));
    }
    let nonce = crate::control::new_request(crate::control::Command::Status {})?.request_id;
    let temp = parent.join(format!(".mcp-{nonce}.tmp"));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)?;
        serde_json::to_writer_pretty(&mut file, &config)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        fs::rename(&temp, path)?;
        Ok(())
    })();
    let _ = fs::remove_file(temp);
    result
}

pub fn connection_config(executable: &Path, config_root: &Path, runtime: &Path) -> String {
    serde_json::to_string_pretty(&serde_json::json!({"mcpServers":{"super-desktop":{"command":executable,"args":["mcp","serve"],"env":{"XDG_CONFIG_HOME":config_root,"XDG_RUNTIME_DIR":runtime}}}})).expect("configuration serializes")
}
pub fn connection_instructions(executable: &Path, config_root: &Path, runtime: &Path) -> String {
    format!("Configure your MCP client to launch a local stdio server named super-desktop.\nExecutable: {}\nArguments: [\"mcp\", \"serve\"]\nEnvironment: XDG_CONFIG_HOME={} and XDG_RUNTIME_DIR={}\nUse your client's MCP configuration format. The client launches the server itself; do not start it in a separate terminal.\nOnce connected, discover its tools and check app_status. SUPER DESKTOP must be running for desktop queries. Settings → MCP controls available tools. This connection is local only.", executable.display(), config_root.display(), runtime.display())
}
pub fn executable() -> io::Result<PathBuf> {
    let app = std::env::current_exe()?;
    let client = app.with_file_name("super-desktop-client");
    Ok(if client.is_file() { client } else { app })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    #[test]
    fn mcp_settings_persist_and_invalid_files_fail_closed() {
        let nonce = crate::control::new_request(crate::control::Command::Status {})
            .unwrap()
            .request_id;
        let root = std::env::temp_dir().join(format!("sd-mcp-settings-{nonce}"));
        let path = root.join("mcp.json");
        assert_eq!(load_at(&path).unwrap(), Config::default());
        let config = Config {
            enabled: true,
            read_output: true,
            launch: true,
            prompts: true,
        };
        save_at(&path, config).unwrap();
        assert_eq!(load_at(&path).unwrap(), config);
        fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
        save_at(&path, config).unwrap();
        assert_eq!(load_at(&path).unwrap(), config);
        fs::set_permissions(&root, fs::Permissions::from_mode(0o777)).unwrap();
        assert!(load_at(&path).is_err());
        assert!(save_at(&path, Config::disabled()).is_err());
        fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();

        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        save_at(&path, Config::disabled()).unwrap();
        assert!(!load_at(&path).unwrap().allows("app_status"));
        fs::write(&path, b"bad JSON").unwrap();
        assert!(load_at(&path).is_err());
        fs::write(&path, b"{\"enabled\":true}").unwrap();
        assert_eq!(load_at(&path).unwrap(), Config::default());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(load_at(&path).is_err());
        fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(root.join("missing"), &path).unwrap();
        assert!(load_at(&path).is_err());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn mcp_settings_gate_tools_and_configuration_escapes_paths() {
        let config = Config::default();
        assert!(config.allows("app_status"));
        for tool in ["capture_terminal", "launch_harness", "submit_prompt"] {
            assert!(!config.allows(tool));
        }
        let config = Config {
            enabled: false,
            read_output: true,
            launch: true,
            prompts: true,
        };
        for tool in [
            "app_status",
            "capture_terminal",
            "launch_harness",
            "submit_prompt",
        ] {
            assert!(!config.allows(tool));
        }
        let executable = Path::new("/tmp/folder with spaces/agent\"client");
        let config: serde_json::Value = serde_json::from_str(&connection_config(
            executable,
            Path::new("/tmp/config root"),
            Path::new("/tmp/runtime"),
        ))
        .unwrap();
        assert_eq!(
            config["mcpServers"]["super-desktop"]["env"]["XDG_CONFIG_HOME"],
            "/tmp/config root"
        );
        assert_eq!(
            config["mcpServers"]["super-desktop"]["env"]["XDG_RUNTIME_DIR"],
            "/tmp/runtime"
        );
        assert_eq!(
            config["mcpServers"]["super-desktop"]["command"],
            executable.to_str().unwrap()
        );
    }
}
