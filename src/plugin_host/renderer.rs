//! Window renderers: a plugin's WebAssembly module that decides where and how
//! big each local card is drawn (`skills/super-desktop-plugin/references/renderer-abi.md`).
//!
//! Run in process with `wasmi` (an interpreter: no JIT, no system access).
//! The module may import nothing but `env.sd_log`; its memory is capped; each
//! frame gets a fuel budget. Output is matched to the input cards, made
//! finite and kept on screen before anyone draws it. Three failures within 10
//! seconds turn the renderer off. The renderer never changes saved state; only
//! a drop target does, applied by the host like a user's drop.
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use wasmi::{Caller, Config, Engine, Linker, Memory, Module, Store, StoreLimits, StoreLimitsBuilder, TypedFunc};

pub const ABI: u32 = 1;
pub const FUEL: u64 = 1_000_000;
pub const MAX_CARDS: usize = 128;
pub const STATE_CAP: usize = 4096;
/// Settings passed after the cards (`renderer.params`), one f32 each.
pub const MAX_PARAMS: usize = crate::plugin_host::manifest::MAX_RENDERER_PARAMS;
const IN_HEADER: usize = 64;
const IN_CARD: usize = 64;
const IN_CARDS_AT: usize = IN_HEADER + STATE_CAP;
const OUT_HEADER: usize = 48;
const OUT_CARD: usize = 32;
const OUT_CARDS_AT: usize = OUT_HEADER + STATE_CAP;
const IN_MAGIC: u32 = u32::from_le_bytes(*b"SDRI");
const OUT_MAGIC: u32 = u32::from_le_bytes(*b"SDRO");
const MAX_MEMORY: usize = 16 * 1024 * 1024;
const FAILURE_WINDOW: Duration = Duration::from_secs(10);
const MAX_FAILURES: usize = 3;
const MAX_LOGS_PER_SECOND: usize = 20;

pub const FLAG_ICONIFIED: u32 = 1;
pub const FLAG_EXPANDED: u32 = 2;
pub const FLAG_DRAGGING: u32 = 4;
pub const FLAG_FOCUSED: u32 = 8;
pub const FLAG_DROPPED: u32 = 32;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// One card as the renderer sees it.
#[derive(Clone, Debug, Default)]
pub struct CardIn {
    pub id: u32,
    pub flags: u32,
    /// The saved open rectangle (while dragging: where the drag has it).
    pub saved: Rect,
    pub icon_x: f64,
    pub icon_y: f64,
    pub icon_side: f64,
    pub status: u32,
    pub agent_hash: u32,
    pub z: u32,
    pub min_w: f64,
    pub min_h: f64,
}

#[derive(Clone, Debug, Default)]
pub struct Frame {
    pub screen_w: f64,
    pub screen_h: f64,
    pub top: f64,
    pub phase: u32,
    pub time_ms: f64,
    pub dt_ms: f64,
    pub pointer: (f64, f64),
    pub pointer_down: bool,
    pub focused: u32,
    pub cards: Vec<CardIn>,
    /// The plugin's `renderer.params` settings, in manifest order.
    pub params: Vec<f64>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Mode {
    /// 0: the card keeps its own size and is scaled to the rectangle.
    Full,
    /// 1: an icon.
    Icon,
    /// 2: the card is laid out at the rectangle's size (more columns and
    /// rows) once the layout settles; scaled while it moves.
    Resized,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CardOut {
    pub id: u32,
    pub rect: Rect,
    pub mode: Mode,
    pub opacity: f64,
    pub z: u32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DropTarget {
    pub id: u32,
    pub x: f64,
    pub y: f64,
    pub to_icon: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Output {
    pub cards: Vec<CardOut>,
    pub animating: bool,
    pub drop: Option<DropTarget>,
}

struct HostState {
    limits: StoreLimits,
    log: PathBuf,
    logs: VecDeque<Instant>,
}

pub struct Renderer {
    store: Store<HostState>,
    memory: Memory,
    present: TypedFunc<i32, i32>,
    input_at: usize,
    output_at: usize,
    state: Vec<u8>,
    failures: VecDeque<Instant>,
    /// Set once the renderer failed too often; it draws nothing after that.
    pub off: Option<String>,
    /// Fuel the last successful frame used (`plugin test` reports it).
    pub last_fuel: u64,
}

impl std::fmt::Debug for Renderer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Renderer").field("off", &self.off).finish_non_exhaustive()
    }
}

/// A renderer's `params`: its number and bool settings as numbers (a bool is
/// 1 or 0), in manifest order, from the saved settings or their defaults.
pub fn params_of(manifest: &super::manifest::Manifest) -> Vec<f64> {
    let Some(spec) = &manifest.contributes.renderer else { return Vec::new() };
    let values = super::store::settings(manifest);
    spec.params
        .iter()
        .map(|key| match values.get(key) {
            Some(serde_json::Value::Bool(on)) => f64::from(u8::from(*on)),
            Some(value) => value.as_f64().unwrap_or(0.0),
            None => 0.0,
        })
        .collect()
}

/// FNV-1a 32-bit, the ABI's agent hash.
pub fn agent_hash(agent: &str) -> u32 {
    agent.bytes().fold(0x811c9dc5u32, |h, b| (h ^ u32::from(b)).wrapping_mul(0x01000193))
}

impl Renderer {
    /// Load and check a module: imports, exports, ABI version, buffers.
    pub fn load(wasm: &[u8], log: &Path) -> Result<Renderer, String> {
        Self::load_with_params(wasm, log, 0)
    }

    /// `load` for a renderer that gets `params` settings after the cards: its
    /// input buffer must hold them too.
    pub fn load_with_params(wasm: &[u8], log: &Path, params: usize) -> Result<Renderer, String> {
        let mut config = Config::default();
        config.consume_fuel(true);
        let engine = Engine::new(&config);
        let module = Module::new(&engine, wasm).map_err(|e| format!("not a valid WebAssembly module: {e}"))?;
        for import in module.imports() {
            if (import.module(), import.name()) != ("env", "sd_log") {
                return Err(format!("imports `{}.{}`; a renderer may import only env.sd_log", import.module(), import.name()));
            }
        }
        let limits = StoreLimitsBuilder::new().memory_size(MAX_MEMORY).instances(1).build();
        let mut store = Store::new(&engine, HostState { limits, log: log.to_path_buf(), logs: VecDeque::new() });
        store.limiter(|state| &mut state.limits);
        store.set_fuel(FUEL).map_err(|e| e.to_string())?;
        let mut linker = <Linker<HostState>>::new(&engine);
        linker
            .func_wrap("env", "sd_log", |caller: Caller<'_, HostState>, ptr: i32, len: i32| {
                sd_log(caller, ptr, len);
            })
            .map_err(|e| e.to_string())?;
        let instance = linker.instantiate_and_start(&mut store, &module).map_err(|e| format!("cannot start the module: {e}"))?;
        let memory = instance.get_memory(&store, "memory").ok_or("no exported `memory`")?;
        let typed = |name: &str| instance.get_typed_func::<(), i32>(&store, name).map_err(|e| format!("export `{name}`: {e}"));
        let (abi, input, output) = (typed("sd_abi_version")?, typed("sd_input")?, typed("sd_output")?);
        let present = instance.get_typed_func::<i32, i32>(&store, "sd_present").map_err(|e| format!("export `sd_present`: {e}"))?;
        let version = abi.call(&mut store, ()).map_err(|e| format!("sd_abi_version: {e}"))?;
        if version != ABI as i32 {
            return Err(format!("renderer ABI {version}; this build runs ABI {ABI}"));
        }
        let input_at = input.call(&mut store, ()).map_err(|e| format!("sd_input: {e}"))? as u32 as usize;
        let output_at = output.call(&mut store, ()).map_err(|e| format!("sd_output: {e}"))? as u32 as usize;
        let size = memory.data(&store).len();
        if input_at + IN_CARDS_AT + MAX_CARDS * IN_CARD + 4 * params > size || output_at + OUT_CARDS_AT + MAX_CARDS * OUT_CARD > size {
            return Err("sd_input/sd_output buffers do not fit in memory".into());
        }
        Ok(Renderer { store, memory, present, input_at, output_at, state: Vec::new(), failures: VecDeque::new(), off: None, last_fuel: 0 })
    }

    /// One frame. `Err` means "draw the built-in layout this frame"; after
    /// three failures within 10 s the renderer is off for good.
    pub fn present(&mut self, frame: &Frame) -> Result<Output, String> {
        if let Some(why) = &self.off {
            return Err(why.clone());
        }
        let result = self.try_present(frame);
        if let Err(why) = &result {
            let now = Instant::now();
            self.failures.push_back(now);
            while self.failures.front().is_some_and(|t| now.duration_since(*t) > FAILURE_WINDOW) {
                self.failures.pop_front();
            }
            // A failed frame forgets the renderer's state: it restarts clean.
            self.state.clear();
            if self.failures.len() >= MAX_FAILURES {
                self.off = Some(format!("turned off after {MAX_FAILURES} failures in 10 s; last: {why}"));
            }
        }
        result
    }

    fn try_present(&mut self, frame: &Frame) -> Result<Output, String> {
        if frame.cards.len() > MAX_CARDS {
            return Err(format!("more than {MAX_CARDS} cards"));
        }
        let input = encode(frame, &self.state);
        self.memory.write(&mut self.store, self.input_at, &input).map_err(|e| e.to_string())?;
        self.store.set_fuel(FUEL).map_err(|e| e.to_string())?;
        let len = self.present.call(&mut self.store, input.len() as i32).map_err(|e| format!("sd_present: {e}"))?;
        self.last_fuel = FUEL - self.store.get_fuel().unwrap_or(0);
        if len < 0 {
            return Err(format!("sd_present returned {len}"));
        }
        let len = len as usize;
        let expected = OUT_CARDS_AT + frame.cards.len() * OUT_CARD;
        if len < expected {
            return Err(format!("output is {len} bytes; {expected} expected for {} cards", frame.cards.len()));
        }
        let mut out = vec![0u8; expected];
        self.memory.read(&self.store, self.output_at, &mut out).map_err(|e| e.to_string())?;
        let (output, state) = decode(&out, frame)?;
        self.state = state;
        Ok(output)
    }
}

fn sd_log(caller: Caller<'_, HostState>, ptr: i32, len: i32) {
    let now = Instant::now();
    let Some(memory) = caller.get_export("memory").and_then(|e| e.into_memory()) else { return };
    let text = {
        let data = memory.data(&caller);
        let (start, len) = (ptr.max(0) as usize, (len.max(0) as usize).min(1024));
        data.get(start..start + len).map(|bytes| String::from_utf8_lossy(bytes).into_owned())
    };
    let mut caller = caller;
    let state = caller.data_mut();
    while state.logs.front().is_some_and(|t| now.duration_since(*t) > Duration::from_secs(1)) {
        state.logs.pop_front();
    }
    if state.logs.len() >= MAX_LOGS_PER_SECOND {
        return;
    }
    state.logs.push_back(now);
    if let Some(text) = text {
        super::api::append_log(&state.log, "renderer", &text);
    }
}

fn put_u32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}
fn put_f32(b: &mut [u8], at: usize, v: f64) {
    put_u32(b, at, (v as f32).to_bits());
}
fn get_u32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}
fn get_f32(b: &[u8], at: usize) -> f64 {
    f64::from(f32::from_bits(get_u32(b, at)))
}

/// The input frame, byte for byte as in renderer-abi.md.
pub fn encode(frame: &Frame, state: &[u8]) -> Vec<u8> {
    let params = &frame.params[..frame.params.len().min(MAX_PARAMS)];
    let mut b = vec![0u8; IN_CARDS_AT + frame.cards.len() * IN_CARD + 4 * params.len()];
    put_u32(&mut b, 0, IN_MAGIC);
    put_u32(&mut b, 4, ABI);
    put_f32(&mut b, 8, frame.screen_w);
    put_f32(&mut b, 12, frame.screen_h);
    put_f32(&mut b, 16, frame.top);
    put_u32(&mut b, 20, frame.phase);
    b[24..32].copy_from_slice(&frame.time_ms.to_le_bytes());
    put_f32(&mut b, 32, frame.dt_ms);
    put_f32(&mut b, 36, frame.pointer.0);
    put_f32(&mut b, 40, frame.pointer.1);
    put_u32(&mut b, 44, u32::from(frame.pointer_down));
    put_u32(&mut b, 48, frame.focused);
    put_u32(&mut b, 52, frame.cards.len() as u32);
    let state = &state[..state.len().min(STATE_CAP)];
    put_u32(&mut b, 56, state.len() as u32);
    put_u32(&mut b, 60, params.len() as u32);
    b[IN_HEADER..IN_HEADER + state.len()].copy_from_slice(state);
    for (i, card) in frame.cards.iter().enumerate() {
        let at = IN_CARDS_AT + i * IN_CARD;
        put_u32(&mut b, at, card.id);
        put_u32(&mut b, at + 4, card.flags);
        put_f32(&mut b, at + 8, card.saved.x);
        put_f32(&mut b, at + 12, card.saved.y);
        put_f32(&mut b, at + 16, card.saved.w);
        put_f32(&mut b, at + 20, card.saved.h);
        put_f32(&mut b, at + 24, card.icon_x);
        put_f32(&mut b, at + 28, card.icon_y);
        put_f32(&mut b, at + 32, card.icon_side);
        put_u32(&mut b, at + 36, card.status);
        put_u32(&mut b, at + 40, card.agent_hash);
        put_u32(&mut b, at + 44, card.z);
        put_f32(&mut b, at + 48, card.min_w);
        put_f32(&mut b, at + 52, card.min_h);
    }
    let params_at = IN_CARDS_AT + frame.cards.len() * IN_CARD;
    for (i, value) in params.iter().enumerate() {
        put_f32(&mut b, params_at + 4 * i, if value.is_finite() { *value } else { 0.0 });
    }
    b
}

/// Read and sanitize the output: one entry per input card, finite numbers,
/// on screen below the top bar, at least the minimum size.
pub fn decode(b: &[u8], frame: &Frame) -> Result<(Output, Vec<u8>), String> {
    if get_u32(b, 0) != OUT_MAGIC || get_u32(b, 4) != ABI {
        return Err("output magic or ABI is wrong".into());
    }
    if get_u32(b, 8) as usize != frame.cards.len() {
        return Err(format!("output has {} cards; the input had {}", get_u32(b, 8), frame.cards.len()));
    }
    let flags = get_u32(b, 12);
    let state_len = (get_u32(b, 32) as usize).min(STATE_CAP);
    let state = b[OUT_HEADER..OUT_HEADER + state_len].to_vec();
    let mut cards = Vec::with_capacity(frame.cards.len());
    for i in 0..frame.cards.len() {
        let at = OUT_CARDS_AT + i * OUT_CARD;
        let id = get_u32(b, at);
        let input = frame.cards.iter().find(|c| c.id == id).ok_or_else(|| format!("output names card {id}, which is not in the input"))?;
        if cards.iter().any(|c: &CardOut| c.id == id) {
            return Err(format!("card {id} appears twice in the output"));
        }
        let mode = match get_u32(b, at + 20) {
            1 => Mode::Icon,
            2 => Mode::Resized,
            _ => Mode::Full,
        };
        let mut rect = Rect { x: get_f32(b, at + 4), y: get_f32(b, at + 8), w: get_f32(b, at + 12), h: get_f32(b, at + 16) };
        let fallback = input.saved;
        for (value, default) in [(&mut rect.x, fallback.x), (&mut rect.y, fallback.y), (&mut rect.w, fallback.w), (&mut rect.h, fallback.h)] {
            if !value.is_finite() {
                *value = default;
            }
        }
        match mode {
            Mode::Icon => {
                let side = rect.w.min(rect.h).clamp(48.0, 256.0);
                rect.w = side;
                rect.h = side;
            }
            Mode::Full | Mode::Resized => {
                rect.w = rect.w.max(input.min_w.max(1.0));
                rect.h = rect.h.max(input.min_h.max(1.0));
            }
        }
        // Whole on screen where it fits; at least partly where it does not.
        rect.w = rect.w.min(frame.screen_w.max(48.0));
        rect.h = rect.h.min((frame.screen_h - frame.top).max(48.0));
        rect.x = rect.x.clamp(0.0, (frame.screen_w - rect.w).max(0.0));
        rect.y = rect.y.clamp(frame.top, (frame.screen_h - rect.h).max(frame.top));
        let opacity = get_f32(b, at + 24);
        let opacity = if opacity.is_finite() { opacity.clamp(0.2, 1.0) } else { 1.0 };
        cards.push(CardOut { id, rect, mode, opacity, z: get_u32(b, at + 28) });
    }
    let drop = (flags & 2 != 0)
        .then(|| DropTarget { id: get_u32(b, 16), x: get_f32(b, 20), y: get_f32(b, 24), to_icon: get_u32(b, 28) == 1 })
        // Honoured only for the card that was just dropped, and only finite.
        .filter(|d| d.x.is_finite() && d.y.is_finite())
        .filter(|d| frame.cards.iter().any(|c| c.id == d.id && c.flags & FLAG_DROPPED != 0));
    Ok((Output { cards, animating: flags & 1 != 0, drop }, state))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny module in binary form, assembled here so the tests need no
    /// toolchain: it exports the ABI and either echoes input rectangles,
    /// loops forever, or traps (chosen by `body`).
    fn module(present_body: &[u8], extra_import: bool) -> Vec<u8> {
        let mut m = b"\0asm\x01\0\0\0".to_vec();
        let section = |m: &mut Vec<u8>, id: u8, body: Vec<u8>| {
            m.push(id);
            leb(m, body.len() as u32);
            m.extend(body);
        };
        // types: 0 = () -> i32, 1 = (i32) -> i32, 2 = (i32, i32) -> ()
        section(&mut m, 1, vec![3, 0x60, 0, 1, 0x7f, 0x60, 1, 0x7f, 1, 0x7f, 0x60, 2, 0x7f, 0x7f, 0]);
        let funcs_offset = if extra_import { 1 } else { 0 };
        if extra_import {
            let mut body = vec![1];
            for s in ["env", "now"] {
                body.push(s.len() as u8);
                body.extend(s.as_bytes());
            }
            body.extend([0, 2]);
            section(&mut m, 2, body);
        }
        section(&mut m, 3, vec![4, 0, 0, 0, 1]);
        // memory: 1 page min (64 KiB)
        section(&mut m, 5, vec![1, 0, 1]);
        let mut exports = vec![5];
        for (name, kind, index) in [("memory", 2u8, 0u8), ("sd_abi_version", 0, funcs_offset), ("sd_input", 0, funcs_offset + 1), ("sd_output", 0, funcs_offset + 2), ("sd_present", 0, funcs_offset + 3)] {
            exports.push(name.len() as u8);
            exports.extend(name.as_bytes());
            exports.extend([kind, index]);
        }
        section(&mut m, 7, exports);
        let func = |body: &[u8]| {
            let mut f = vec![0u8];
            f.extend(body);
            f.push(0x0b);
            let mut out = Vec::new();
            leb(&mut out, f.len() as u32);
            out.extend(f);
            out
        };
        let mut code = vec![4];
        code.extend(func(&[0x41, 1]));
        code.extend(func(&[0x41, 0]));
        code.extend(func(&[0x41, 0x80, 0x80, 0x02])); // output at 32768
        code.extend(func(present_body));
        section(&mut m, 10, code);
        m
    }

    fn leb(out: &mut Vec<u8>, mut v: u32) {
        loop {
            let byte = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                out.push(byte);
                break;
            }
            out.push(byte | 0x80);
        }
    }

    fn frame() -> Frame {
        Frame {
            screen_w: 1920.0,
            screen_h: 1080.0,
            top: 46.0,
            cards: vec![CardIn { id: 1, saved: Rect { x: 100.0, y: 100.0, w: 640.0, h: 480.0 }, min_w: 320.0, min_h: 200.0, ..Default::default() }],
            ..Default::default()
        }
    }

    #[test]
    fn plugin_renderer_rejects_bad_modules() {
        let log = std::env::temp_dir().join("sd-renderer-test.log");
        assert!(Renderer::load(b"not wasm", &log).unwrap_err().contains("not a valid"));
        let with_import = module(&[0x41, 0], true);
        assert!(Renderer::load(&with_import, &log).unwrap_err().contains("may import only env.sd_log"));
        assert!(Renderer::load(&module(&[0x41, 0], false), &log).is_ok());
    }

    #[test]
    fn plugin_renderer_budget_and_three_strikes() {
        let log = std::env::temp_dir().join("sd-renderer-test.log");
        // `loop br 0 end`: never returns; the fuel budget stops it.
        let mut forever = Renderer::load(&module(&[0x03, 0x40, 0x0c, 0x00, 0x0b, 0x41, 0x00], false), &log).unwrap();
        let started = Instant::now();
        assert!(forever.present(&frame()).unwrap_err().contains("sd_present"));
        assert!(started.elapsed() < Duration::from_secs(2), "the budget stops a runaway frame quickly");
        assert!(forever.off.is_none());
        let _ = forever.present(&frame());
        let _ = forever.present(&frame());
        assert!(forever.off.as_deref().is_some_and(|w| w.contains("3 failures")));
        // `unreachable`: a trap is a failure too.
        let mut trap = Renderer::load(&module(&[0x00], false), &log).unwrap();
        assert!(trap.present(&frame()).is_err());
        // Returning nothing useful (0 bytes) is refused, not drawn.
        let mut empty = Renderer::load(&module(&[0x41, 0x00], false), &log).unwrap();
        assert!(empty.present(&frame()).unwrap_err().contains("bytes"));
    }

    #[test]
    fn plugin_renderer_params_follow_the_cards() {
        let mut f = frame();
        let n = f.cards.len();
        let plain = encode(&f, &[]);
        assert_eq!((plain.len(), get_u32(&plain, 60)), (IN_CARDS_AT + n * IN_CARD, 0), "no params: the ABI 1 frame as before");
        f.params = vec![70.0, 1.0, f64::NAN];
        let b = encode(&f, &[]);
        assert_eq!(get_u32(&b, 60), 3);
        assert_eq!(b.len(), IN_CARDS_AT + n * IN_CARD + 12);
        let at = IN_CARDS_AT + n * IN_CARD;
        assert_eq!((get_f32(&b, at), get_f32(&b, at + 4), get_f32(&b, at + 8)), (70.0, 1.0, 0.0));
        assert_eq!(b[64..IN_CARDS_AT + n * IN_CARD], plain[64..], "state and cards unchanged");
    }

    #[test]
    fn plugin_renderer_output_is_sanitized() {
        let f = frame();
        let mut out = vec![0u8; OUT_CARDS_AT + OUT_CARD];
        put_u32(&mut out, 0, OUT_MAGIC);
        put_u32(&mut out, 4, ABI);
        put_u32(&mut out, 8, 1);
        put_u32(&mut out, 12, 2 | 1);
        put_u32(&mut out, 16, 1);
        put_u32(&mut out, OUT_CARDS_AT, 1);
        put_f32(&mut out, OUT_CARDS_AT + 4, f64::NAN);
        put_f32(&mut out, OUT_CARDS_AT + 8, -500.0);
        put_f32(&mut out, OUT_CARDS_AT + 12, 99999.0);
        put_f32(&mut out, OUT_CARDS_AT + 16, 10.0);
        put_f32(&mut out, OUT_CARDS_AT + 24, 5.0);
        let (output, _) = decode(&out, &f).unwrap();
        let card = output.cards[0];
        assert_eq!(card.rect.x, 0.0, "NaN x → saved x, then kept on screen");
        assert_eq!(card.rect.y, 46.0, "never above the top bar");
        assert_eq!(card.rect.w, 1920.0, "never wider than the screen");
        assert_eq!(card.rect.h, 200.0, "never below the minimum height");
        assert_eq!(card.opacity, 1.0);
        assert!(output.animating);
        assert!(output.drop.is_none(), "a drop target only for a card that was just dropped");
        put_u32(&mut out, OUT_CARDS_AT, 7);
        assert!(decode(&out, &f).unwrap_err().contains("not in the input"));
        put_u32(&mut out, 8, 2);
        assert!(decode(&out, &f).unwrap_err().contains("2 cards"));
    }

    /// Frame time of the real example renderer with 128 cards, in an optimized
    /// build: `cargo test --release plugin_renderer_frame_time -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn plugin_renderer_frame_time() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("skills/super-desktop-plugin/examples/gravity-wm/renderer.wasm");
        let wasm = std::fs::read(&path).expect("build gravity-wm first");
        let mut renderer = Renderer::load(&wasm, &std::env::temp_dir().join("sd-renderer-bench.log")).unwrap();
        let mut f = frame();
        f.dt_ms = 16.0;
        f.cards = (0..128)
            .map(|i| CardIn {
                id: i + 1,
                flags: if i % 5 == 0 { FLAG_ICONIFIED } else { 0 },
                saved: Rect { x: f64::from(i * 13 % 1800), y: f64::from(80 + i * 7 % 900), w: 640.0, h: 480.0 },
                icon_x: f64::from(i * 17 % 1800),
                icon_y: f64::from(80 + i * 11 % 900),
                min_w: 320.0,
                min_h: 200.0,
                ..Default::default()
            })
            .collect();
        renderer.present(&f).unwrap();
        let frames = 500;
        let started = Instant::now();
        for _ in 0..frames {
            renderer.present(&f).unwrap();
        }
        let per_frame = started.elapsed() / frames;
        eprintln!("gravity-wm, 128 cards: {per_frame:?} per frame");
        assert!(per_frame < Duration::from_millis(4), "{per_frame:?}");
    }

    /// The real example renderer, when it has been built (`build.sh`).
    #[test]
    fn plugin_renderer_runs_gravity_wm_when_built() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("skills/super-desktop-plugin/examples/gravity-wm/renderer.wasm");
        let Ok(wasm) = std::fs::read(&path) else {
            eprintln!("skipping: {} is not built", path.display());
            return;
        };
        let mut renderer = Renderer::load(&wasm, &std::env::temp_dir().join("sd-renderer-test.log")).unwrap();
        let mut f = frame();
        f.dt_ms = 16.0;
        f.focused = 1;
        f.cards[0].flags = FLAG_FOCUSED;
        f.cards[0].saved = Rect { x: 640.0, y: 300.0, w: 640.0, h: 480.0 };
        let mut last = None;
        for _ in 0..200 {
            last = Some(renderer.present(&f).unwrap());
        }
        let out = last.unwrap();
        assert!(!out.animating);
        assert!(out.cards[0].rect.w > 1000.0, "the centre card is larger: {:?}", out.cards[0]);
        assert_eq!(out.cards[0].mode, Mode::Resized, "and really resized, not zoomed");
    }
}
