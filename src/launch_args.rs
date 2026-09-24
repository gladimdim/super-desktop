//! Starting parameters for built-in harnesses (⚙ Settings → Harness launchers
//! → Parameters).
//!
//! A built-in harness starts with its `AgentConfig::default_args` until the
//! user saves a list of their own, which then replaces those defaults: an
//! empty list starts it with no parameters at all. The daemon installs the
//! saved lists at startup and again on every save, so each launch path (top
//! bar, phone, another PC) and each restore resolves the same arguments in
//! `tmux::resolve_command`. Processes that never install any — the bridge,
//! tests — keep the built-in defaults.
use std::collections::BTreeMap;
use std::sync::RwLock;

static SAVED: RwLock<BTreeMap<String, Vec<String>>> = RwLock::new(BTreeMap::new());

/// Make `saved` (`AppState::harness_args`) the parameters new cards start with.
pub fn install(saved: &BTreeMap<String, Vec<String>>) {
    *SAVED.write().unwrap_or_else(|poisoned| poisoned.into_inner()) = saved.clone();
}

/// The parameters `key` starts with when the user saved none.
pub fn builtin(key: &str) -> Vec<String> {
    crate::tmux::get_agent_config(key)
        .default_args
        .iter()
        .map(|arg| (*arg).to_owned())
        .collect()
}

/// The parameters a new `key` card starts with.
pub fn effective(key: &str) -> Vec<String> {
    SAVED
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(key)
        .cloned()
        .unwrap_or_else(|| builtin(key))
}

/// Remember `arguments` for `key`. Saving exactly the built-in defaults drops
/// the entry, so a later change to those defaults still reaches this harness.
pub fn store(saved: &mut BTreeMap<String, Vec<String>>, key: &str, arguments: Vec<String>) {
    if arguments == builtin(key) {
        saved.remove(key);
    } else {
        saved.insert(key.to_owned(), arguments);
    }
}

/// The settings field, split like a shell would: quotes keep words together.
pub fn parse(text: &str) -> Result<Vec<String>, String> {
    let arguments = shlex::split(text).ok_or("Parameters have an unmatched quote")?;
    validate(&arguments)?;
    Ok(arguments)
}

pub fn validate(arguments: &[String]) -> Result<(), String> {
    if arguments.len() > 32
        || arguments
            .iter()
            .any(|arg| arg.len() > 1024 || arg.chars().any(char::is_control))
    {
        return Err("Use at most 32 arguments of up to 1024 characters each".into());
    }
    Ok(())
}

/// One argument as a word for tmux's command shell. Plain flags stay as they
/// are, so the built-in defaults read exactly as before.
pub fn quote(arg: &str) -> String {
    shlex::try_quote(arg)
        .map(|quoted| quoted.into_owned())
        .unwrap_or_else(|_| format!("'{}'", arg.replace('\'', "'\"'\"'")))
}

/// `arguments` as the settings field shows them: quoted only where needed.
pub fn display(arguments: &[String]) -> String {
    arguments.iter().map(|arg| quote(arg)).collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(list: &[&str]) -> Vec<String> {
        list.iter().map(|word| (*word).to_owned()).collect()
    }

    #[test]
    fn parse_keeps_quoted_words_and_rejects_bad_input() {
        assert_eq!(
            parse("--model opus --append-system-prompt 'be brief'").unwrap(),
            words(&["--model", "opus", "--append-system-prompt", "be brief"])
        );
        assert_eq!(parse("   ").unwrap(), Vec::<String>::new());
        assert_eq!(parse("--x 'open").unwrap_err(), "Parameters have an unmatched quote");
        assert!(parse(&"--a ".repeat(33)).is_err());
        assert!(parse(&"x".repeat(1025)).is_err());
        assert!(validate(&words(&["tab\there"])).is_err());
    }

    #[test]
    fn display_round_trips_through_parse() {
        let arguments = words(&["--model", "opus", "be brief", "it's", "$HOME", ""]);
        assert_eq!(parse(&display(&arguments)).unwrap(), arguments);
        assert_eq!(display(&words(&["--tui-mode", "regular"])), "--tui-mode regular");
    }

    #[test]
    fn storing_the_builtin_defaults_forgets_the_override() {
        let mut saved = BTreeMap::new();
        store(&mut saved, "claude", words(&["--model", "opus"]));
        assert_eq!(saved["claude"], words(&["--model", "opus"]));
        store(&mut saved, "claude", Vec::new());
        assert_eq!(saved["claude"], Vec::<String>::new(), "empty means no parameters");
        store(&mut saved, "claude", builtin("claude"));
        assert!(!saved.contains_key("claude"));
    }
}
