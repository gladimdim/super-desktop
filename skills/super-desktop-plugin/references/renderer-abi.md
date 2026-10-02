# Window renderer ABI 1

A renderer decides where, how big and in which form every **local** card is
drawn, on every frame. It is a WebAssembly module (`wasm32`, MVP features)
that the host runs in-process with `wasmi`. It cannot do I/O, cannot allocate
from the host and cannot run past its budget.

Working example: `examples/center-magnify/src/lib.rs` (Rust, `no_std` on
wasm32, with native unit tests). Start from it.

## What the host guarantees and enforces

- **The saved layout belongs to the user.** Input gives each card's saved
  rectangle (`TerminalData`). The renderer's output is only *drawn*; it is
  never saved. When the renderer is turned off, the built-in rules draw the
  saved layout again, animated. The only way to change what is saved is
  `drop_target` (below), applied like a user drag.
- **Budget:** 1 000 000 fuel (roughly instructions) per call. Running out, a
  trap, a negative return, a wrong magic/version/count, or a missing card in
  the output makes the host use the built-in result for that frame. Three
  failures within 10 seconds turn the renderer off and show a notice.
- **Sanitising:** NaN/∞ become the built-in value. Each rectangle is clamped
  into the canvas below the top bar, full cards to at least `min_w × min_h`,
  icons to 48–256 px squares. Every card stays at least partly visible and
  clickable. Opacity is clamped to 0.2–1.0.
- **How a rectangle is drawn:** a full card keeps its own size (its
  terminal is never resized, so it keeps its columns and rows) and is scaled
  uniformly to fit the rectangle, centred in it. An icon is laid out as an
  icon of side `min(width, height)`. Opacity is applied as given.
- **What stays built-in:** an expanded card, and every card while the
  show/hide slide runs (phases 3 and 4 are not sent by this build). Draw
  order (`z`) is not applied by this build: cards keep their stacking order,
  and the focused or dragged card is on top.
- **Pointer:** while a card is dragged, `pointer` is the centre of its saved
  rectangle and bit 0 of the flags is set; otherwise both are 0.
- **Calls:** single-threaded, one call at a time, only while something moves
  (a drag, an unsettled animation, a layout change). When the output says
  `animating = 0`, the host stops calling until something changes.
- **Remote workspaces** (another PC's cards) never use a plugin renderer.

## Module contract

Exports (all required):

| Export | Signature | Meaning |
| --- | --- | --- |
| `memory` | memory | Linear memory, at most 16 MiB. |
| `sd_abi_version` | `() -> i32` | Must return `1`. |
| `sd_input` | `() -> i32` | Address of an input buffer of at least 12 352 bytes. |
| `sd_output` | `() -> i32` | Address of an output buffer of at least 8 240 bytes. |
| `sd_present` | `(input_len: i32) -> i32` | Reads the input frame, writes the output frame, returns the output length, or a negative error. |

Imports: none, or only `env.sd_log(ptr: i32, len: i32)` (UTF-8 text into the
plugin log, at most 20 calls per second). A module with any other import is
refused at install.

Per frame the host writes the input at `sd_input()`, calls
`sd_present(len)`, and reads `len` bytes at `sd_output()`.

Negative returns used by the example: `-1` bad length, `-2` bad magic or ABI,
`-3` too many cards. Any negative value is treated as a failure.

## Byte layout

All numbers little-endian. `f32` is IEEE-754. Offsets in bytes. Unused and
reserved bytes are zero on input and must be zero on output.

### Input frame

Header, 64 bytes:

| Offset | Type | Field |
| --- | --- | --- |
| 0 | u32 | magic `"SDRI"` (`0x49524453`) |
| 4 | u32 | ABI version, `1` |
| 8 | f32 | screen width (logical px) |
| 12 | f32 | screen height |
| 16 | f32 | top: bottom edge of the top bar; cards live below it |
| 20 | u32 | phase: 0 idle, 1 drag, 2 arrange, 3 show, 4 hide |
| 24 | f64 | time in ms (monotonic) |
| 32 | f32 | ms since the previous call (0 on the first call) |
| 36 | f32 | pointer x |
| 40 | f32 | pointer y |
| 44 | u32 | flags: bit 0 pointer button down |
| 48 | u32 | focused card id, 0 for none |
| 52 | u32 | card count `n`, at most 128 (more cards: the built-in renderer is used) |
| 56 | u32 | state length (≤ 4096) |
| 60 | u32 | params count `p` (0–16; 0 unless the manifest lists `renderer.params`) |

State, 4096 bytes at offset 64: exactly the bytes the previous output
returned as state (empty on the first call, after a restart, and after
activation). Use it for animation positions and velocities.

Cards, `n` records of 64 bytes at offset 4160:

| Offset | Type | Field |
| --- | --- | --- |
| 0 | u32 | id: a non-zero handle, stable while the daemon runs |
| 4 | u32 | flags: bit 0 iconified, bit 1 expanded, bit 2 dragging, bit 3 focused, bit 5 dropped (set on the one frame the user let go of it) |
| 8 | f32 | saved x (while dragging: the live drag position) |
| 12 | f32 | saved y |
| 16 | f32 | saved width |
| 20 | f32 | saved height |
| 24 | f32 | icon x (the saved icon spot) |
| 28 | f32 | icon y |
| 32 | f32 | built-in icon side |
| 36 | u32 | status: 0 unknown, 1 working, 2 idle, 3 waiting, 4 completed, 5 error |
| 40 | u32 | agent hash: FNV-1a 32-bit of the harness key (`claude`, `codex`, …) |
| 44 | u32 | stacking order, 0 = bottom |
| 48 | f32 | minimum width for a full card |
| 52 | f32 | minimum height for a full card |
| 56 | 8 bytes | reserved |

Params, `p` f32 values right after the cards (offset 4160 + 64 × n): the
settings named in `renderer.params`, in that order. See [Params](#params).

Input length = 4160 + 64 × n + 4 × p.

### Output frame

Header, 48 bytes:

| Offset | Type | Field |
| --- | --- | --- |
| 0 | u32 | magic `"SDRO"` (`0x4F524453`) |
| 4 | u32 | ABI version, `1` |
| 8 | u32 | card count: must equal the input count |
| 12 | u32 | flags: bit 0 animating (call me again next frame), bit 1 drop target present |
| 16 | u32 | drop target: card id |
| 20 | f32 | drop target: x |
| 24 | f32 | drop target: y |
| 28 | u32 | drop target: 1 = save as an icon with its icon spot at (x, y); 0 = save as an open card at (x, y) (an icon is opened) |
| 32 | u32 | state length (≤ 4096) |
| 36 | 12 bytes | reserved |

State, 4096 bytes at offset 48 (only `state length` bytes are kept).

Cards, `n` records of 32 bytes at offset 4144, one per input card, any order:

| Offset | Type | Field |
| --- | --- | --- |
| 0 | u32 | id |
| 4 | f32 | x |
| 8 | f32 | y |
| 12 | f32 | width |
| 16 | f32 | height |
| 20 | u32 | mode: 0 full card, 1 icon (drawn as a square of `min(width, height)`) |
| 24 | f32 | opacity |
| 28 | u32 | draw order: higher draws on top; ties keep the input stacking order |

Output length = 4144 + 32 × n.

### Params

A manifest's `renderer.params` lists up to 16 setting keys of type `number`
or `bool`. Every frame carries their current values (the user's, else the
default) as f32 after the cards; a bool is 1 or 0. A change in Settings →
Plugins reaches the next frame and wakes the renderer. Settings that are all
params need no process.

- Declare a range (`min`, `max`) and a `default` for each number setting, and
  still check the values in the renderer: fall back to your default when a
  value is missing (`p` smaller than you expect, as on an older host) or out
  of range.
- With params, the input buffer must hold them too: at least
  12 352 + 4 × 16 bytes is safe for every manifest. `sd_present` must accept
  an input length up to that.

### Drop target

Only honoured for a card whose input had the *dropped* flag in the same frame
(the frame right after the user let go; the host has already saved the drop
where the pointer left it).
It replaces where the host would save the drop. The host saves it through the
same path as a user drag, so it survives turning the renderer off. Use it to
make the drawn result and the saved layout agree (for example: dropped in the
edge band → saved as an icon at the edge).

## Writing one

- Rust: `crate-type = ["cdylib", "rlib"]`, `#![cfg_attr(target_arch = "wasm32", no_std)]`,
  static input/output buffers, a `#[panic_handler]` for wasm32 only, and the
  pure `present(&[u8], &mut [u8]) -> Result<usize, i32>` function tested
  natively with `cargo test`. Build with
  `cargo build --release --target wasm32-unknown-unknown` (`opt-level = "s"`,
  `lto`, `panic = "abort"`). The example is about 7 KB.
- Other languages: Zig, C (clang `--target=wasm32`), AssemblyScript and
  TinyGo work if the module has the exports above and no other imports.
  Avoid runtimes that need WASI or a host allocator.
- Do not rely on `f32::sqrt`, `exp`, `sin` and the like under `no_std`: older
  toolchains do not have them in `core`. Write `abs`, `clamp` and `smoothstep`
  yourself, and smooth with `alpha = dt / (tau + dt)` instead of `exp`.
- Keep per-frame work linear or `n log n` in the card count; 128 cards must fit
  in the budget. The example takes about 0.1 ms per frame for 128 cards in an
  optimized SUPER DESKTOP build. The example uses about 270 000 fuel for 128 cards; looking up
  each card's state with a linear scan instead of by slot pushes that past
  850 000.
- Start a card that is new to your state at its saved rectangle so activation
  animates from the built-in layout instead of jumping. The first frame after
  activation or a toggle has `dt = 0`: treat it as one ordinary frame step,
  or the cards jump.
- Give users a way to switch the effect off without turning the plugin off:
  a command with `"action": "renderer.toggle"` (manifest.md#commands) on a
  toolbar item and a shortcut.
- Treat the dragged card specially: keep it under the pointer, and animate only
  its size.
