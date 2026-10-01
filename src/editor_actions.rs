//! Editor actions from the phone: when a terminal's foreground program is
//! nano, vim/vi/nvim or emacs, the phone offers Save, Close, Save & close,
//! Discard & close, Discard changes, New file and scrolling, and this module
//! turns each into that editor's own keystrokes.
//!
//! The PC, not the phone, does the translation, so it can check that the
//! editor is still in the foreground at the moment it types: `:q!⏎` must never
//! land in a shell or an AI harness's prompt after the editor has exited.
use std::time::Duration;

/// The editors that have actions, by the key language they speak.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// nano with its default bindings (^O/^X/^C).
    Nano,
    /// nano started with `--modernbindings` (^S/^Q/^C copies).
    NanoModern,
    Vim,
    /// A vi that is not vim (nvi, busybox): no `Ctrl-\ Ctrl-N`.
    Vi,
    Emacs,
}

impl Kind {
    /// The name the phone sees and sends back.
    pub fn wire(self) -> &'static str {
        match self {
            Kind::Nano | Kind::NanoModern => "nano",
            Kind::Vim => "vim",
            Kind::Vi => "vi",
            Kind::Emacs => "emacs",
        }
    }

    /// What this PC can do in this editor, in the order the phone lists them.
    pub fn actions(self) -> &'static [&'static str] {
        match self {
            Kind::Vim | Kind::Vi => &["save", "quit", "saveQuit", "discardQuit", "revert", "newFile", "scroll"],
            Kind::Nano | Kind::NanoModern => &["save", "quit", "saveQuit", "discardQuit", "scroll"],
            Kind::Emacs => &["save", "quit", "saveQuit", "newFile", "scroll"],
        }
    }
}

/// An editor in a pane's foreground, as sent in terminal stream frames.
/// Fields in key order, like the rest of the frame.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Detected {
    pub actions: &'static [&'static str],
    /// The foreground program's name, for display only.
    pub command: String,
    pub kind: &'static str,
}

/// The editor a pane is running, from tmux's `pane_current_command`, whether
/// the pane is on the alternate screen, and the foreground process's
/// executable name and arguments when they could be read. Full-screen editors
/// switch to the alternate screen, so a script-mode `vim -es` or a pane still
/// starting up does not count.
pub fn classify(command: &str, alternate: bool, exe: Option<&str>, args: &[String]) -> Option<Kind> {
    if !alternate {
        return None;
    }
    let name = command.trim();
    // `comm` is at most 15 bytes: `emacs-30.1`, `emacs-nox`, `vim.basic`.
    let vim_like = |n: &str| n.starts_with("vim") || n.starts_with("nvim") || matches!(n, "view" | "rvim" | "vimdiff" | "rview");
    if name == "nano" || name == "rnano" {
        let modern = args.iter().skip(1).any(|arg| arg == "--modernbindings" || arg == "-/");
        return Some(if modern { Kind::NanoModern } else { Kind::Nano });
    }
    if vim_like(name) {
        return Some(Kind::Vim);
    }
    if name == "vi" || name == "nvi" || name == "ex" {
        // `vi` is often vim under another name.
        let exe_is_vim = exe.is_some_and(vim_like);
        return Some(if exe_is_vim { Kind::Vim } else { Kind::Vi });
    }
    if name.starts_with("emacs") {
        return Some(Kind::Emacs);
    }
    None
}

/// The foreground process of a pane whose first process is `pane_pid`, from
/// the terminal's foreground process group.
fn foreground_pid(pane_pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pane_pid}/stat")).ok()?;
    let (_, fields) = stat.rsplit_once(')')?;
    let foreground = fields.split_whitespace().nth(5)?.parse::<u32>().ok()?;
    Some(if foreground == 0 { pane_pid } else { foreground })
}

/// Executable file name and arguments of the pane's foreground process.
fn foreground_process(pane_pid: u32) -> (Option<String>, Vec<String>) {
    let Some(pid) = foreground_pid(pane_pid) else { return (None, Vec::new()) };
    let exe = std::fs::read_link(format!("/proc/{pid}/exe"))
        .ok()
        .and_then(|path| path.file_name().map(|name| name.to_string_lossy().into_owned()));
    let args = std::fs::read(format!("/proc/{pid}/cmdline"))
        .map(|raw| {
            raw.split(|byte| *byte == 0)
                .filter(|arg| !arg.is_empty())
                .take(64)
                .map(|arg| String::from_utf8_lossy(arg).into_owned())
                .collect()
        })
        .unwrap_or_default();
    (exe, args)
}

/// The editor in the foreground of a pane, reading `/proc` only when the
/// command is an editor name that needs it (vi and nano).
pub fn detect(command: &str, alternate: bool, pane_pid: &str) -> Option<Detected> {
    let first = classify(command, alternate, None, &[])?;
    let kind = match first {
        Kind::Vi | Kind::Nano => {
            let (exe, args) = pane_pid.trim().parse().map(foreground_process).unwrap_or_default();
            classify(command, alternate, exe.as_deref(), &args).unwrap_or(first)
        }
        other => other,
    };
    Some(Detected { actions: kind.actions(), command: command.trim().to_string(), kind: kind.wire() })
}

/// What the phone asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Save,
    Quit,
    SaveQuit,
    DiscardQuit,
    Revert,
    NewFile(String),
    /// Toward the end of the file when `down`; by lines or by pages.
    Scroll { down: bool, amount: Amount },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Amount {
    Lines(u16),
    Pages(u16),
}

pub const MAX_SCROLL_LINES: u16 = 100;
pub const MAX_SCROLL_PAGES: u16 = 10;
pub const MAX_FILE_NAME: usize = 255;

impl Action {
    fn name(&self) -> &'static str {
        match self {
            Action::Save => "save",
            Action::Quit => "quit",
            Action::SaveQuit => "saveQuit",
            Action::DiscardQuit => "discardQuit",
            Action::Revert => "revert",
            Action::NewFile(_) => "newFile",
            Action::Scroll { .. } => "scroll",
        }
    }

    /// The action in a request body: `{"action", "fileName"?, "direction"?,
    /// "lines"?, "pages"?}`. Unknown fields are ignored.
    pub fn parse(body: &serde_json::Value) -> Result<Self, Error> {
        let action = body.get("action").and_then(|v| v.as_str()).ok_or(Error::InvalidRequest)?;
        Ok(match action {
            "save" => Action::Save,
            "quit" => Action::Quit,
            "saveQuit" => Action::SaveQuit,
            "discardQuit" => Action::DiscardQuit,
            "revert" => Action::Revert,
            "newFile" => {
                let name = body.get("fileName").and_then(|v| v.as_str()).ok_or(Error::InvalidFileName)?;
                if name.is_empty()
                    || name.len() > MAX_FILE_NAME
                    || name.trim() != name
                    || name.chars().any(char::is_control)
                {
                    return Err(Error::InvalidFileName);
                }
                Action::NewFile(name.to_string())
            }
            "scroll" => {
                let down = match body.get("direction").and_then(|v| v.as_str()) {
                    Some("down") => true,
                    Some("up") => false,
                    _ => return Err(Error::InvalidRequest),
                };
                let count = |key: &str, max: u16| -> Result<Option<u16>, Error> {
                    match body.get(key) {
                        None | Some(serde_json::Value::Null) => Ok(None),
                        Some(value) => value
                            .as_u64()
                            .filter(|n| (1..=u64::from(max)).contains(n))
                            .map(|n| Some(n as u16))
                            .ok_or(Error::InvalidRequest),
                    }
                };
                let amount = match (count("lines", MAX_SCROLL_LINES)?, count("pages", MAX_SCROLL_PAGES)?) {
                    (Some(_), Some(_)) => return Err(Error::InvalidRequest),
                    (_, Some(pages)) => Amount::Pages(pages),
                    (Some(lines), None) => Amount::Lines(lines),
                    (None, None) => Amount::Lines(3),
                };
                Action::Scroll { down, amount }
            }
            _ => return Err(Error::UnsupportedAction),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidRequest,
    InvalidFileName,
    UnsupportedAction,
    /// The pane's foreground is no longer the editor the phone saw.
    NotForeground(String),
    Tmux(String),
}

impl Error {
    pub fn code(&self) -> &str {
        match self {
            Error::InvalidRequest => "invalid_request",
            Error::InvalidFileName => "invalid_file_name",
            Error::UnsupportedAction => "unsupported_action",
            Error::NotForeground(_) => "editor_not_foreground",
            Error::Tmux(error) => error,
        }
    }
}

/// How a pane takes mouse input, when its program asked for any.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mouse {
    Off,
    /// X10-style `ESC [ M` reports.
    Normal,
    /// `ESC [ < … M` reports.
    Sgr,
}

/// A pane as tmux reports it just before keys are sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaneState {
    pub command: String,
    pub alternate: bool,
    /// Scrolled back in tmux's copy mode: keys would go to tmux, not the editor.
    pub in_mode: bool,
    pub mouse: Mouse,
    pub width: u16,
    pub height: u16,
    pub pid: String,
}

/// The `display-message` format `parse_state` reads. The command comes last,
/// so a name with spaces cannot shift the numeric fields.
pub const STATE_FORMAT: &str = "#{alternate_on} #{pane_in_mode} #{mouse_any_flag} #{mouse_sgr_flag} #{pane_width} #{pane_height} #{pane_pid} #{pane_current_command}";

pub fn parse_state(text: &str) -> Option<PaneState> {
    let line = text.lines().next()?;
    let mut fields = line.splitn(8, ' ');
    let mut flag = || fields.next().map(|f| f.trim() == "1");
    let alternate = flag()?;
    let in_mode = flag()?;
    let any = flag()?;
    let sgr = flag()?;
    let width = fields.next()?.trim().parse().ok()?;
    let height = fields.next()?.trim().parse().ok()?;
    let pid = fields.next()?.trim().to_string();
    let command = fields.next().unwrap_or("").trim().to_string();
    let mouse = match (any, sgr) {
        (false, _) => Mouse::Off,
        (true, true) => Mouse::Sgr,
        (true, false) => Mouse::Normal,
    };
    Some(PaneState { command, alternate, in_mode, mouse, width, height, pid })
}

/// One thing to do to the pane, in order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// Bytes typed as one input.
    Keys(Vec<u8>),
    /// Give the editor time to react, then stop (successfully) unless the same
    /// editor is still in the foreground: a quit that needed no answer must not
    /// have its answer typed into whatever runs next.
    Settle,
}

const ESC: u8 = 0x1b;
const fn ctrl(key: u8) -> u8 {
    key & 0x1f
}

fn keys(bytes: impl Into<Vec<u8>>) -> Step {
    Step::Keys(bytes.into())
}

/// vim's `fnameescape()`: a file name typed after `:e` stays one literal name.
fn vim_file_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 8);
    for (i, c) in name.chars().enumerate() {
        if " \t\n*?[{`$\\%#'\"|!<".contains(c) || (i == 0 && matches!(c, '+' | '>' | '-')) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// The editor command line `text⏎`, typed after returning to Normal mode.
fn vim_command(kind: Kind, text: &str) -> Vec<Step> {
    let mut steps = vim_normal(kind);
    steps.push(keys(format!(":{text}\r")));
    steps
}

/// Normal mode from any mode: `Ctrl-\ Ctrl-N` in vim, Esc alone in other vis
/// (sent as its own input, so it is not read as Meta with the next key).
fn vim_normal(kind: Kind) -> Vec<Step> {
    if kind == Kind::Vim {
        vec![keys([ctrl(b'\\'), ctrl(b'n')])]
    } else {
        vec![keys([ESC])]
    }
}

/// Mouse wheel reports at the pane's middle: what a wheel on the PC sends.
fn wheel(state: &PaneState, down: bool, notches: u16) -> Vec<u8> {
    let (x, y) = (state.width / 2 + 1, state.height / 2 + 1);
    let button = if down { 65u16 } else { 64 };
    let one = match state.mouse {
        Mouse::Sgr => format!("\x1b[<{button};{x};{y}M").into_bytes(),
        _ => {
            // X10 coordinates are single bytes offset by 32.
            let (x, y) = (x.min(223) as u8, y.min(223) as u8);
            vec![ESC, b'[', b'M', 32 + button as u8, 32 + x, 32 + y]
        }
    };
    one.repeat(notches as usize)
}

/// Lines one wheel notch scrolls in these editors by default.
const WHEEL_LINES: u16 = 3;

fn page_key(down: bool) -> &'static [u8] {
    if down { b"\x1b[6~" } else { b"\x1b[5~" }
}

fn scroll(kind: Kind, state: &PaneState, down: bool, amount: Amount) -> Vec<Step> {
    let page = state.height.saturating_sub(2).max(1);
    let lines = match amount {
        Amount::Lines(lines) => lines,
        Amount::Pages(pages) => pages.saturating_mul(page),
    };
    // A program that takes the mouse scrolls by the wheel without changing
    // its mode or moving the cursor.
    if state.mouse != Mouse::Off {
        return vec![keys(wheel(state, down, lines.div_ceil(WHEEL_LINES)))];
    }
    let pages = match amount {
        Amount::Pages(pages) => Some(pages),
        Amount::Lines(lines) if lines >= page => Some(lines / page),
        Amount::Lines(_) => None,
    };
    match (kind, pages) {
        // PgUp/PgDn scroll a page in Normal and Insert mode, and in nano.
        (Kind::Vim | Kind::Nano | Kind::NanoModern, Some(pages)) => vec![keys(page_key(down).repeat(pages as usize))],
        (Kind::Vi, Some(pages)) => {
            let mut steps = vim_normal(kind);
            steps.push(keys(format!("{pages}").into_bytes().into_iter().chain([ctrl(if down { b'f' } else { b'b' })]).collect::<Vec<_>>()));
            steps
        }
        // Ctrl-E / Ctrl-Y scroll the view by lines, from Normal mode.
        (Kind::Vim | Kind::Vi, None) => {
            let mut steps = vim_normal(kind);
            let key = ctrl(if down { b'e' } else { b'y' });
            steps.push(keys(format!("{lines}").into_bytes().into_iter().chain([key]).collect::<Vec<_>>()));
            steps
        }
        // nano scrolls by moving the cursor past the edge of the screen.
        (Kind::Nano | Kind::NanoModern, None) => {
            vec![keys((if down { b"\x1b[B" } else { b"\x1b[A" }).repeat(lines as usize))]
        }
        // C-u N C-v / C-u N M-v scroll the view N lines.
        (Kind::Emacs, pages) => {
            let lines = pages.map_or(lines, |pages| pages.saturating_mul(page));
            let mut bytes = vec![ctrl(b'u')];
            bytes.extend(lines.to_string().bytes());
            if down {
                bytes.push(ctrl(b'v'));
            } else {
                bytes.extend([ESC, b'v']);
            }
            vec![keys(bytes)]
        }
    }
}

/// The steps that perform `action` in `kind`, on a pane in `state`.
pub fn plan(kind: Kind, action: &Action, state: &PaneState) -> Result<Vec<Step>, Error> {
    if !kind.actions().contains(&action.name()) {
        return Err(Error::UnsupportedAction);
    }
    if let Action::Scroll { down, amount } = action {
        return Ok(scroll(kind, state, *down, *amount));
    }
    Ok(match kind {
        Kind::Vim | Kind::Vi => match action {
            Action::Save => vim_command(kind, "w"),
            Action::Quit => vim_command(kind, "q"),
            Action::SaveQuit => vim_command(kind, "wq"),
            Action::DiscardQuit => vim_command(kind, "q!"),
            Action::Revert => vim_command(kind, "e!"),
            Action::NewFile(name) => vim_command(kind, &format!("e {}", vim_file_name(name))),
            Action::Scroll { .. } => unreachable!(),
        },
        // ^C cancels a prompt (and only reports the cursor position otherwise).
        // Save is ^S in both binding sets; a buffer without a name asks for one.
        Kind::Nano => match action {
            Action::Save => vec![keys([ctrl(b'c')]), keys([ctrl(b's')])],
            Action::Quit => vec![keys([ctrl(b'c')]), keys([ctrl(b'x')])],
            Action::SaveQuit => vec![keys([ctrl(b'c')]), keys([ctrl(b's')]), Step::Settle, keys([ctrl(b'x')])],
            Action::DiscardQuit => vec![keys([ctrl(b'c')]), keys([ctrl(b'x')]), Step::Settle, keys(*b"n")],
            _ => return Err(Error::UnsupportedAction),
        },
        // ^C copies with modern bindings, so there is no reset step.
        Kind::NanoModern => match action {
            Action::Save => vec![keys([ctrl(b's')])],
            Action::Quit => vec![keys([ctrl(b'q')])],
            Action::SaveQuit => vec![keys([ctrl(b's')]), Step::Settle, keys([ctrl(b'q')])],
            Action::DiscardQuit => vec![keys([ctrl(b'q')]), Step::Settle, keys(*b"n")],
            _ => return Err(Error::UnsupportedAction),
        },
        // C-g leaves a minibuffer or a half-typed key sequence.
        Kind::Emacs => match action {
            Action::Save => vec![keys([ctrl(b'g')]), keys([ctrl(b'x'), ctrl(b's')])],
            Action::Quit => vec![keys([ctrl(b'g')]), keys([ctrl(b'x'), ctrl(b'c')])],
            // With a prefix argument, save every file buffer without asking, then exit.
            Action::SaveQuit => vec![keys([ctrl(b'g')]), keys([ctrl(b'u'), ctrl(b'x'), ctrl(b'c')])],
            Action::NewFile(name) => {
                let mut bytes = vec![ctrl(b'x'), ctrl(b'f')];
                bytes.extend(name.replace('$', "$$").bytes());
                bytes.push(b'\r');
                vec![keys([ctrl(b'g')]), keys(bytes)]
            }
            _ => return Err(Error::UnsupportedAction),
        },
    })
}

/// A terminal pane keys can be typed into.
pub trait Pane {
    fn state(&mut self) -> Result<PaneState, String>;
    fn leave_copy_mode(&mut self) -> Result<(), String>;
    fn type_bytes(&mut self, bytes: &[u8]) -> Result<(), String>;
}

/// Time between inputs, so an editor reads a lone Esc as Esc.
const BETWEEN_INPUTS: Duration = Duration::from_millis(40);
/// Time an editor gets to quit or show its question.
const SETTLE: Duration = Duration::from_millis(250);

/// The editor in `state`'s foreground, the way frames report it.
fn kind_of(state: &PaneState) -> Option<Kind> {
    let (exe, args) = state.pid.parse().map(foreground_process).unwrap_or_default();
    classify(&state.command, state.alternate, exe.as_deref(), &args)
}

/// Perform `action` in `pane` if it still runs the editor the phone saw
/// (`expected`, a wire name), and return that editor's wire name.
pub fn run(pane: &mut dyn Pane, expected: &str, action: &Action) -> Result<&'static str, Error> {
    let state = pane.state().map_err(Error::Tmux)?;
    let kind = kind_of(&state).filter(|kind| kind.wire() == expected).ok_or_else(|| Error::NotForeground(state.command.clone()))?;
    let steps = plan(kind, action, &state)?;
    if state.in_mode {
        pane.leave_copy_mode().map_err(Error::Tmux)?;
    }
    for (i, step) in steps.iter().enumerate() {
        match step {
            Step::Keys(bytes) => {
                if i > 0 {
                    std::thread::sleep(BETWEEN_INPUTS);
                }
                pane.type_bytes(bytes).map_err(Error::Tmux)?;
            }
            Step::Settle => {
                std::thread::sleep(SETTLE);
                let now = pane.state().map_err(Error::Tmux)?;
                if kind_of(&now) != Some(kind) {
                    break;
                }
            }
        }
    }
    Ok(kind.wire())
}

impl Pane for crate::tmux_control::Control {
    fn state(&mut self) -> Result<PaneState, String> {
        parse_state(&self.pane_format(STATE_FORMAT)?).ok_or_else(|| "tmux_state_unreadable".into())
    }
    fn leave_copy_mode(&mut self) -> Result<(), String> {
        self.cancel_copy_mode()
    }
    fn type_bytes(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.send_bytes(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn state(mouse: Mouse) -> PaneState {
        PaneState { command: "vim".into(), alternate: true, in_mode: false, mouse, width: 80, height: 24, pid: String::new() }
    }

    fn bytes(steps: &[Step]) -> Vec<u8> {
        steps
            .iter()
            .flat_map(|step| match step {
                Step::Keys(bytes) => bytes.clone(),
                Step::Settle => b"<settle>".to_vec(),
            })
            .collect()
    }

    #[test]
    fn editor_actions_recognize_editors_by_their_foreground_program() {
        let args = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        for (command, kind) in [
            ("vim", Kind::Vim),
            ("nvim", Kind::Vim),
            ("vim.basic", Kind::Vim),
            ("vimdiff", Kind::Vim),
            ("view", Kind::Vim),
            ("nano", Kind::Nano),
            ("rnano", Kind::Nano),
            ("emacs", Kind::Emacs),
            ("emacs-30.1", Kind::Emacs),
            ("emacsclient", Kind::Emacs),
            ("vi", Kind::Vi),
            ("nvi", Kind::Vi),
        ] {
            assert_eq!(classify(command, true, None, &[]), Some(kind), "{command}");
            // Not full screen: script mode or still starting.
            assert_eq!(classify(command, false, None, &[]), None, "{command}");
        }
        assert_eq!(classify("vi", true, Some("vim"), &[]), Some(Kind::Vim));
        assert_eq!(classify("vi", true, Some("busybox"), &[]), Some(Kind::Vi));
        assert_eq!(classify("nano", true, None, &args(&["nano", "--modernbindings", "a"])), Some(Kind::NanoModern));
        assert_eq!(classify("nano", true, None, &args(&["nano", "-/"])), Some(Kind::NanoModern));
        assert_eq!(classify("nano", true, None, &args(&["nano", "--", "-/"])), Some(Kind::NanoModern));
        for other in ["bash", "zsh", "tmux", "claude", "node", "less", "htop", "ssh", ""] {
            assert_eq!(classify(other, true, None, &[]), None, "{other}");
        }
    }

    #[test]
    fn editor_actions_frame_field_lists_only_what_the_editor_supports() {
        let detected = detect("nvim", true, "").unwrap();
        assert_eq!(
            serde_json::to_value(&detected).unwrap(),
            json!({"kind":"vim","command":"nvim","actions":["save","quit","saveQuit","discardQuit","revert","newFile","scroll"]})
        );
        assert_eq!(detect("nano", true, "").unwrap().actions, ["save", "quit", "saveQuit", "discardQuit", "scroll"]);
        assert_eq!(detect("emacs", true, "").unwrap().actions, ["save", "quit", "saveQuit", "newFile", "scroll"]);
        assert!(detect("bash", false, "1").is_none());
        // Every listed action has a plan; nothing else does.
        let all = [
            Action::Save,
            Action::Quit,
            Action::SaveQuit,
            Action::DiscardQuit,
            Action::Revert,
            Action::NewFile("a.md".into()),
            Action::Scroll { down: true, amount: Amount::Lines(3) },
        ];
        for kind in [Kind::Nano, Kind::NanoModern, Kind::Vim, Kind::Vi, Kind::Emacs] {
            for action in &all {
                let listed = kind.actions().contains(&action.name());
                assert_eq!(plan(kind, action, &state(Mouse::Off)).is_ok(), listed, "{kind:?} {action:?}");
            }
        }
    }

    #[test]
    fn editor_actions_parse_requests_strictly() {
        assert_eq!(Action::parse(&json!({"action":"save","editor":"vim","extra":1})), Ok(Action::Save));
        assert_eq!(Action::parse(&json!({"action":"newFile","fileName":"notes/a b.md"})), Ok(Action::NewFile("notes/a b.md".into())));
        for name in [json!(""), json!(" a"), json!("a\nb"), json!("a\u{1b}b"), json!("x".repeat(256)), json!(5), serde_json::Value::Null] {
            assert_eq!(Action::parse(&json!({"action":"newFile","fileName":name})), Err(Error::InvalidFileName), "{name}");
        }
        assert_eq!(
            Action::parse(&json!({"action":"scroll","direction":"down"})),
            Ok(Action::Scroll { down: true, amount: Amount::Lines(3) })
        );
        assert_eq!(
            Action::parse(&json!({"action":"scroll","direction":"up","pages":2})),
            Ok(Action::Scroll { down: false, amount: Amount::Pages(2) })
        );
        for bad in [
            json!({"action":"scroll"}),
            json!({"action":"scroll","direction":"left"}),
            json!({"action":"scroll","direction":"up","lines":0}),
            json!({"action":"scroll","direction":"up","lines":101}),
            json!({"action":"scroll","direction":"up","pages":11}),
            json!({"action":"scroll","direction":"up","lines":2,"pages":1}),
            json!({"action":"scroll","direction":"up","lines":"2"}),
            json!({}),
            json!({"action":3}),
        ] {
            assert_eq!(Action::parse(&bad), Err(Error::InvalidRequest), "{bad}");
        }
        assert_eq!(Action::parse(&json!({"action":"format"})), Err(Error::UnsupportedAction));
    }

    #[test]
    fn editor_actions_translate_into_each_editors_keys() {
        let off = state(Mouse::Off);
        let vim = |action: Action| bytes(&plan(Kind::Vim, &action, &off).unwrap());
        assert_eq!(vim(Action::Save), b"\x1c\x0e:w\r");
        assert_eq!(vim(Action::SaveQuit), b"\x1c\x0e:wq\r");
        assert_eq!(vim(Action::DiscardQuit), b"\x1c\x0e:q!\r");
        assert_eq!(vim(Action::Revert), b"\x1c\x0e:e!\r");
        assert_eq!(vim(Action::NewFile("my notes|x %.md".into())), b"\x1c\x0e:e my\\ notes\\|x\\ \\%.md\r");
        assert_eq!(vim(Action::NewFile("+cmd".into())), b"\x1c\x0e:e \\+cmd\r");
        // Plain vi gets Esc on its own, before the command.
        let vi = plan(Kind::Vi, &Action::Quit, &off).unwrap();
        assert_eq!(vi, vec![Step::Keys(vec![0x1b]), Step::Keys(b":q\r".to_vec())]);
        let nano = |kind, action: Action| bytes(&plan(kind, &action, &off).unwrap());
        assert_eq!(nano(Kind::Nano, Action::Save), b"\x03\x13");
        assert_eq!(nano(Kind::Nano, Action::DiscardQuit), b"\x03\x18<settle>n");
        assert_eq!(nano(Kind::Nano, Action::SaveQuit), b"\x03\x13<settle>\x18");
        assert_eq!(nano(Kind::NanoModern, Action::Quit), b"\x11");
        assert_eq!(nano(Kind::NanoModern, Action::DiscardQuit), b"\x11<settle>n");
        let emacs = |action: Action| bytes(&plan(Kind::Emacs, &action, &off).unwrap());
        assert_eq!(emacs(Action::Save), b"\x07\x18\x13");
        assert_eq!(emacs(Action::SaveQuit), b"\x07\x15\x18\x03");
        assert_eq!(emacs(Action::NewFile("a$HOME.txt".into())), b"\x07\x18\x06a$$HOME.txt\r");
    }

    #[test]
    fn editor_actions_scroll_by_wheel_when_the_editor_takes_the_mouse() {
        let down3 = Action::Scroll { down: true, amount: Amount::Lines(3) };
        let up7 = Action::Scroll { down: false, amount: Amount::Lines(7) };
        // SGR wheel reports at the pane's middle, one notch per three lines.
        assert_eq!(bytes(&plan(Kind::Vim, &down3, &state(Mouse::Sgr)).unwrap()), b"\x1b[<65;41;13M");
        assert_eq!(bytes(&plan(Kind::Emacs, &up7, &state(Mouse::Sgr)).unwrap()), b"\x1b[<64;41;13M".repeat(3));
        assert_eq!(bytes(&plan(Kind::Nano, &down3, &state(Mouse::Normal)).unwrap()), [0x1b, b'[', b'M', 32 + 65, 32 + 41, 32 + 13]);
        // A page is the pane's height less two lines of context.
        let page = Action::Scroll { down: true, amount: Amount::Pages(1) };
        assert_eq!(bytes(&plan(Kind::Vim, &page, &state(Mouse::Sgr)).unwrap()), b"\x1b[<65;41;13M".repeat(8));
    }

    #[test]
    fn editor_actions_scroll_by_keys_without_the_mouse() {
        let off = state(Mouse::Off);
        let lines = |down, n| Action::Scroll { down, amount: Amount::Lines(n) };
        let pages = |down, n| Action::Scroll { down, amount: Amount::Pages(n) };
        assert_eq!(bytes(&plan(Kind::Vim, &lines(true, 5), &off).unwrap()), b"\x1c\x0e5\x05");
        assert_eq!(bytes(&plan(Kind::Vim, &lines(false, 2), &off).unwrap()), b"\x1c\x0e2\x19");
        // Pages work in Insert mode too.
        assert_eq!(bytes(&plan(Kind::Vim, &pages(true, 2), &off).unwrap()), b"\x1b[6~\x1b[6~");
        assert_eq!(bytes(&plan(Kind::Vim, &lines(false, 44), &off).unwrap()), b"\x1b[5~\x1b[5~");
        assert_eq!(bytes(&plan(Kind::Vi, &pages(false, 1), &off).unwrap()), b"\x1b1\x02");
        assert_eq!(bytes(&plan(Kind::Nano, &lines(true, 2), &off).unwrap()), b"\x1b[B\x1b[B");
        assert_eq!(bytes(&plan(Kind::Nano, &pages(false, 1), &off).unwrap()), b"\x1b[5~");
        assert_eq!(bytes(&plan(Kind::Emacs, &lines(true, 4), &off).unwrap()), b"\x154\x16");
        assert_eq!(bytes(&plan(Kind::Emacs, &pages(false, 1), &off).unwrap()), b"\x1522\x1bv");
    }

    #[test]
    fn editor_actions_parse_tmux_state() {
        let state = parse_state("1 0 1 1 120 40 4242 nvim\n").unwrap();
        assert_eq!(state, PaneState { command: "nvim".into(), alternate: true, in_mode: false, mouse: Mouse::Sgr, width: 120, height: 40, pid: "4242".into() });
        assert_eq!(parse_state("0 1 1 0 80 24 1 my editor").unwrap().command, "my editor");
        assert_eq!(parse_state("0 1 1 0 80 24 1 x").unwrap().mouse, Mouse::Normal);
        assert_eq!(parse_state("0 0 0 0 80 24 1 bash").unwrap().mouse, Mouse::Off);
        assert!(parse_state("").is_none());
        assert!(parse_state("1 0 1 1 wide 40 1 vim").is_none());
    }

    /// A pane in a private tmux server, never the user's.
    struct TestPane {
        server: String,
        target: String,
    }

    impl TestPane {
        fn tmux(&self, args: &[&str]) -> std::process::Output {
            std::process::Command::new(crate::tmux::tmux_bin())
                .args(["-L", &self.server, "-f", "/dev/null"])
                .args(args)
                .output()
                .unwrap()
        }
        fn screen(&self) -> String {
            String::from_utf8_lossy(&self.tmux(&["capture-pane", "-p", "-t", &self.target]).stdout).into_owned()
        }
        fn wait(&mut self, what: &str, until: impl Fn(&mut Self) -> bool) {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while !until(self) {
                assert!(std::time::Instant::now() < deadline, "timed out waiting for {what}:\n{}", self.screen());
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }

    impl Drop for TestPane {
        fn drop(&mut self) {
            self.tmux(&["kill-server"]);
        }
    }

    impl Pane for TestPane {
        fn state(&mut self) -> Result<PaneState, String> {
            let out = self.tmux(&["display-message", "-p", "-t", &self.target, STATE_FORMAT]);
            parse_state(&String::from_utf8_lossy(&out.stdout)).ok_or_else(|| "unreadable".into())
        }
        fn leave_copy_mode(&mut self) -> Result<(), String> {
            self.tmux(&["send-keys", "-t", &self.target, "-X", "cancel"]);
            Ok(())
        }
        fn type_bytes(&mut self, bytes: &[u8]) -> Result<(), String> {
            let hex: Vec<String> = bytes.iter().map(|b| format!("{b:02x}")).collect();
            let mut args = vec!["send-keys", "-t", &self.target, "-H"];
            args.extend(hex.iter().map(String::as_str));
            let out = self.tmux(&args);
            out.status.success().then_some(()).ok_or_else(|| String::from_utf8_lossy(&out.stderr).into_owned())
        }
    }

    fn installed(program: &str) -> bool {
        std::process::Command::new("sh")
            .args(["-c", &format!("command -v {program}")])
            .output()
            .is_ok_and(|out| out.status.success())
    }

    /// A shell in a private tmux server, in a fresh directory.
    fn shell(name: &str) -> (TestPane, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("sd-editor-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut pane = TestPane { server: format!("sd-editor-{}-{name}", std::process::id()), target: "t".into() };
        pane.tmux(&["new-session", "-d", "-s", "t", "-x", "100", "-y", "30", "-c", dir.to_str().unwrap(), &format!("env -i PATH=/usr/bin:/bin TERM=xterm-256color HOME={} sh", dir.display())]);
        pane.wait("the shell", |p| p.state().is_ok_and(|s| s.command == "sh"));
        (pane, dir)
    }

    fn start(pane: &mut TestPane, command: &str, expect: Kind) {
        pane.tmux(&["send-keys", "-t", "t", "-l", command]);
        pane.tmux(&["send-keys", "-t", "t", "Enter"]);
        pane.wait(command, |p| p.state().ok().and_then(|s| kind_of(&s)) == Some(expect));
        std::thread::sleep(Duration::from_millis(300));
    }

    fn back_at_shell(pane: &mut TestPane) {
        pane.wait("the shell again", |p| p.state().is_ok_and(|s| s.command == "sh" && !s.alternate));
    }

    #[test]
    fn editor_actions_drive_real_vim_and_nvim() {
        for (editor, args) in [("vim", "-u NONE -N"), ("nvim", "--clean")] {
            if !installed(editor) {
                eprintln!("SKIPPED: {editor} is not installed");
                continue;
            }
            let (mut pane, dir) = shell(editor);
            // Save & close from Insert mode, with text typed but unsaved.
            start(&mut pane, &format!("{editor} {args} a.txt"), Kind::Vim);
            pane.type_bytes(b"ihello").unwrap();
            assert_eq!(run(&mut pane, "vim", &Action::SaveQuit), Ok("vim"));
            back_at_shell(&mut pane);
            assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "hello\n");
            // Discard & close throws edits away.
            start(&mut pane, &format!("{editor} {args} a.txt"), Kind::Vim);
            pane.type_bytes(b"ggOdiscarded").unwrap();
            assert_eq!(run(&mut pane, "vim", &Action::DiscardQuit), Ok("vim"));
            back_at_shell(&mut pane);
            assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "hello\n");
            // Save keeps it open; Discard changes reverts to what was saved;
            // New file opens another buffer; Close leaves.
            start(&mut pane, &format!("{editor} {args} a.txt"), Kind::Vim);
            pane.type_bytes(b"Asaved").unwrap();
            assert_eq!(run(&mut pane, "vim", &Action::Save), Ok("vim"));
            pane.wait("the save", |_| std::fs::read_to_string(dir.join("a.txt")).unwrap() == "hellosaved\n");
            pane.type_bytes(b"Areverted").unwrap();
            assert_eq!(run(&mut pane, "vim", &Action::Revert), Ok("vim"));
            assert_eq!(run(&mut pane, "vim", &Action::NewFile("new file.txt".into())), Ok("vim"));
            pane.type_bytes(b"inew").unwrap();
            assert_eq!(run(&mut pane, "vim", &Action::SaveQuit), Ok("vim"));
            back_at_shell(&mut pane);
            assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "hellosaved\n");
            assert_eq!(std::fs::read_to_string(dir.join("new file.txt")).unwrap(), "new\n");
            // Once the editor is gone, nothing is typed into the shell.
            assert_eq!(run(&mut pane, "vim", &Action::DiscardQuit), Err(Error::NotForeground("sh".into())));
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn editor_actions_scroll_real_vim_and_nvim() {
        for (editor, args, mouse) in [("vim", "-u NONE -N -c 'set mouse=a'", Mouse::Normal), ("vim", "-u NONE -N", Mouse::Off), ("nvim", "--clean", Mouse::Sgr)] {
            if !installed(editor) {
                eprintln!("SKIPPED: {editor} is not installed");
                continue;
            }
            let (mut pane, dir) = shell(&format!("scroll-{editor}-{}", mouse != Mouse::Off));
            let text: String = (1..=200).map(|n| format!("line {n}\n")).collect();
            std::fs::write(dir.join("long.txt"), text).unwrap();
            start(&mut pane, &format!("{editor} {args} long.txt"), Kind::Vim);
            assert_eq!(pane.state().unwrap().mouse != Mouse::Off, mouse != Mouse::Off, "{editor} {args}");
            let top = |p: &TestPane| p.screen().lines().next().unwrap_or("").trim().to_string();
            assert_eq!(top(&pane), "line 1");
            run(&mut pane, "vim", &Action::Scroll { down: true, amount: Amount::Lines(6) }).unwrap();
            pane.wait("scrolling down", |p| top(p) == "line 7");
            run(&mut pane, "vim", &Action::Scroll { down: false, amount: Amount::Lines(3) }).unwrap();
            pane.wait("scrolling up", |p| top(p) == "line 4");
            run(&mut pane, "vim", &Action::Scroll { down: true, amount: Amount::Pages(1) }).unwrap();
            pane.wait("a page down", |p| top(p).trim_start_matches("line ").parse::<u32>().is_ok_and(|n| n >= 20));
            run(&mut pane, "vim", &Action::DiscardQuit).unwrap();
            back_at_shell(&mut pane);
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn editor_actions_drive_real_nano() {
        if !installed("nano") {
            eprintln!("SKIPPED: nano is not installed");
            return;
        }
        let (mut pane, dir) = shell("nano");
        // Unmodified: Discard & close leaves at once, and its "n" answer must
        // not reach the shell.
        std::fs::write(dir.join("a.txt"), "hello\n").unwrap();
        start(&mut pane, "nano --ignorercfiles a.txt", Kind::Nano);
        assert_eq!(run(&mut pane, "nano", &Action::DiscardQuit), Ok("nano"));
        back_at_shell(&mut pane);
        std::thread::sleep(Duration::from_millis(300));
        assert!(!pane.screen().lines().any(|line| line.trim_end().ends_with("$ n")), "{}", pane.screen());
        // Modified: Discard & close answers nano's question with No.
        start(&mut pane, "nano --ignorercfiles a.txt", Kind::Nano);
        pane.type_bytes(b"changed ").unwrap();
        assert_eq!(run(&mut pane, "nano", &Action::DiscardQuit), Ok("nano"));
        back_at_shell(&mut pane);
        assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "hello\n");
        // Save & close keeps the edit.
        start(&mut pane, "nano --ignorercfiles a.txt", Kind::Nano);
        pane.type_bytes(b"kept ").unwrap();
        assert_eq!(run(&mut pane, "nano", &Action::SaveQuit), Ok("nano"));
        back_at_shell(&mut pane);
        assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "kept hello\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn editor_actions_drive_real_emacs() {
        if !installed("emacs") {
            eprintln!("SKIPPED: emacs is not installed");
            return;
        }
        let (mut pane, dir) = shell("emacs");
        std::fs::write(dir.join("a.txt"), "hello\n").unwrap();
        start(&mut pane, "emacs -nw -Q a.txt", Kind::Emacs);
        pane.type_bytes(b"kept ").unwrap();
        assert_eq!(run(&mut pane, "emacs", &Action::Save), Ok("emacs"));
        pane.wait("the save", |_| std::fs::read_to_string(dir.join("a.txt")).unwrap() == "kept hello\n");
        assert_eq!(run(&mut pane, "emacs", &Action::NewFile("b.txt".into())), Ok("emacs"));
        pane.type_bytes(b"new").unwrap();
        assert_eq!(run(&mut pane, "emacs", &Action::SaveQuit), Ok("emacs"));
        back_at_shell(&mut pane);
        assert_eq!(std::fs::read_to_string(dir.join("b.txt")).unwrap(), "new");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
