# Referenced files

Open **Files** in a Linux or Android terminal. The list is built on demand from
the current terminal capture: plain filenames, quoted paths, and Markdown link
targets. **Add / Add path** supplies a workspace-relative path if a filename was
wrapped, ambiguous, or already scrolled away. Referenced does not mean generated
by this agent. Refresh returns from a preview to the list.

## Supported previews

- PNG, JPEG, WebP: image preview with zoom; Android supports pinch/pan.
- GIF: animated only while its viewer is open. Large animations are rejected
  before decoding (200 frames / 16 million cumulative canvas pixels).
- PDF: one rasterized page at a time; Previous/Next navigate pages. There is no
  page-count discovery yet; advancing past the last page reports unavailable.
- Markdown: native-text headings and fenced code; other Markdown remains literal
  in this first version. HTML and image/link directives do not execute or fetch.
- Text/source/JSON/YAML/CSV/etc.: selectable text, with a 64K-character preview.

Android's **Share** action explicitly downloads the original and opens
the system share chooser, using a read-only FileProvider URI from a private cache
subdirectory. It is not a public bridge URL. Cache cleanup runs on subsequent
shares (one-hour age / approximately 48 MiB including the newest file). Revoking
a device cannot retract a file already exported to another app.

## Boundaries

Only supported regular files inside the terminal's recorded launch workspace are
eligible. Absolute paths are accepted only within that same workspace. Hidden
components, symlinks, hard links, parent traversal, URLs, special files, and
unsupported formats (including HTML/SVG/archives) are rejected. Old cards without
a recorded workspace must be recreated with a workspace before using Files.
There is no automatic HOME fallback and no recursive scan, even for a workspace
that is itself the home directory. Files outside the workspace cannot currently
be added; no extra-directory approval workflow exists yet.

The in-memory reference history holds at most 64 files for each of 64 terminals
per process and is cleared on restart. Linux and bridge maintain independent
histories; they discover the same terminal output, but manually added entries
are not synchronized between the two processes in this first version.

Content is re-opened with descriptor-relative, no-symlink traversal and checked
against the listed inode/device/size/mtime/ctime before and after reading. Changed
files require refreshing. This is a safety boundary for file delivery, not a
sandbox for approved phones: terminal access already operates as the desktop user.

Files are limited to 16 MiB (512 KiB for text); four asset jobs per process.
No bytes/thumbnails are embedded in terminal WebSocket frames and no asset scan
runs during startup or typing. Downloads, file lookup, image decoding, and PDF
rasterization happen off the UI threads. Closing a viewer cancels Android
downloads and prevents late results from appearing in a different preview;
already running Linux decoding jobs finish bounded work and discard stale results.

## PDF sandbox

Linux uses `/usr/bin/bwrap` and `/usr/bin/pdftoppm`, requiring working unprivileged
user namespaces. It mounts `/usr` and font configuration read-only, private
temporary storage and proc/dev, with no host home, no networking, no capabilities,
and an empty environment. PDF bytes are passed on stdin, page PNGs on stdout.
Limits: 768 MiB address space, five CPU seconds, eight seconds wall time, 1600px
page extent, and 8 MiB output. Failure never retries outside the sandbox.

Android uses the existing pinned/authenticated bridge to fetch those page images,
so it does not run a PDF parser in the credential-bearing application process.
PDF parsing is contained, not independently security-audited; image decoders
still run in the client process and rely on platform decoder updates and limits.

## Verification

- `cargo test --bins`
- `cargo test --bin super-desktop sandbox_renders_a_single_page_fixture -- --ignored`
- `python tests/bridge_security_smoke.py target/release/super-desktop`
- Android: `./gradlew :app:testDebugUnitTest :app:lintDebug :app:assembleDebug`

The Linux sandbox test uses a generated, blank, one-page PDF, not user files.
Device testing is still needed for Fold layout, animated GIF lifecycle, real
large images, PDF navigation, and terminal input latency during previews.
