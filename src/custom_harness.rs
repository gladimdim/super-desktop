//! User-defined launchers are local, explicit commands. The executable and
//! each argument are quoted separately before tmux's command shell sees them.
use serde::{Deserialize, Serialize};
use std::path::Path;

pub const ICONS: [&str; 8] = ["⚡", "🤖", "🔮", "🚀", "🧭", "🧠", "🌌", "💻"];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomHarness {
    pub id: String,
    pub name: String,
    pub icon: String,
    pub executable: String,
    #[serde(default)]
    pub arguments: Vec<String>,
}

impl CustomHarness {
    pub fn create(
        name: &str,
        icon: &str,
        executable: &str,
        arguments: &str,
    ) -> Result<Self, String> {
        let name = name.trim();
        let executable = executable.trim();
        let arguments = shlex::split(arguments).ok_or("Arguments have an unmatched quote")?;
        let result = Self {
            id: format!(
                "custom-{:x}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            ),
            name: name.to_owned(),
            icon: icon.to_owned(),
            executable: executable.to_owned(),
            arguments,
        };
        result.validate()?;
        Ok(result)
    }

    pub fn validate(&self) -> Result<(), String> {
        if !self.id.starts_with("custom-")
            || self.id.len() > 64
            || !self
                .id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err("Invalid launcher ID".into());
        }
        if self.name.is_empty()
            || self.name.chars().count() > 48
            || self.name.chars().any(char::is_control)
        {
            return Err("Enter a name of up to 48 characters".into());
        }
        if !ICONS.contains(&self.icon.as_str()) {
            return Err("Choose one of the eight icons".into());
        }
        if !Path::new(&self.executable).is_absolute() || self.executable.len() > 4096 {
            return Err("Enter an absolute executable path".into());
        }
        if self.arguments.len() > 32
            || self
                .arguments
                .iter()
                .any(|arg| arg.len() > 1024 || arg.chars().any(char::is_control))
        {
            return Err("Use at most 32 arguments of up to 1024 characters each".into());
        }
        if !self.available() {
            return Err("The path is not an executable file".into());
        }
        Ok(())
    }

    pub fn available(&self) -> bool {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(&self.executable)
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }

    pub fn command(&self) -> String {
        std::iter::once(&self.executable)
            .chain(self.arguments.iter())
            .map(|part| format!("'{}'", part.replace('\'', "'\"'\"'")))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn command_quotes_every_argument() {
        let item = CustomHarness::create("My AI", "🤖", "/bin/echo", "--model 'a b' \"it's good\"")
            .unwrap();
        assert_eq!(item.arguments, ["--model", "a b", "it's good"]);
        let output = std::process::Command::new("sh")
            .args(["-c", &item.command()])
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            "--model a b it's good\n"
        );
    }
    #[test]
    fn rejects_missing_or_relative_executable() {
        assert!(CustomHarness::create("AI", "🤖", "echo", "").is_err());
        assert!(CustomHarness::create("AI", "🤖", "/missing/executable", "").is_err());
    }
}
