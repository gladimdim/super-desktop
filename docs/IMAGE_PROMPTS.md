# Images in Android prompts

In an Android Codex terminal, tap **＋**, choose an image, type the prompt and tap
**Send**. A thumbnail shows the selected image; **✕** removes it before sending.
One image and optional text are submitted as one Codex user turn. Image-only
prompts are supported. Other agents are not yet supported by this native-input
adapter; plain text sending is unchanged.

Android uses the system document picker without broad photo/storage permission.
Reading, decoding, resizing, compression and JSON encoding stay off the UI thread.
Source files are capped at 20 MiB and 100 megapixels; output is a still JPEG,
at most 2048px per side and 2 MiB. Orientation is handled by Android's decoder,
transparency is composited on white, animation uses a still frame, and photo
metadata is not uploaded. This is visual prompt input, not original-file transfer.
Selections are kept in memory; switching terminals or destroying the screen
clears the image. A failed send keeps the text and image while the screen remains.

## Native submission

The bridge accepts only a registered, idle Codex terminal running the Codex CLI,
with an empty composer. Existing drafts and menus are not cleared. The current
adapter recognizes the Codex 0.154 composer (empty input or its dim placeholder);
unknown UI versions fail closed and may require an adapter update.

It stages a validated image, bracket-pastes its local path, waits for the real
`[Image #1]` attachment in the composer, bracket-pastes the prompt, then sends
Enter once. It does not merely append a path and ask the agent to read it, restart
the agent, create another chat, or use the desktop clipboard. Until the image is
confirmed there is no Enter. A failure can leave an unsent remote draft: inspect
it before retrying. Do not edit the same terminal from the desktop while sending.
Phone key input is serialized against image submission inside this bridge.

Official guidance establishes interactive image input; the exact bracketed-paste
behavior was additionally verified on local Codex CLI 0.154.0 using a disposable
tmux terminal without submitting an AI request:
https://learn.chatgpt.com/docs/image-inputs?surface=cli

## Security and storage

`POST /api/v1/harnesses/{id}/image-prompt` takes JSON:
`{"requestId":"32 hex characters","text":"Describe this","imageBase64":"..."}`.

- Pinned HTTPS, bearer authorization, origin rejection, revocation checks and
  normal paired-device privileges. No public upload endpoint or client file path.
- Authentication happens before accepting the larger body. Only this route gets
  a 3 MiB JSON body allowance and a 30-second upload deadline; other requests keep
  their 16 KiB limit and initial five-second deadline. Headers remain 16 KiB.
- Uploads share the four-job asset limit, acquired before reading the large body.
- Prompt text is capped at 4096 UTF-8 bytes. Control characters other than newline
  and tab are rejected, so payloads cannot escape bracketed paste.
- Only real PNG/JPEG data is accepted. Decoded dimensions are checked and pixels
  are re-encoded into a fresh PNG without copying metadata. No SVG or arbitrary
  file uploads. Decoders run in-process, not in a sandbox.
- Files are generated privately under
  `~/.local/state/super-desktop/prompt-images/` (directory 0700, files 0600), using
  exclusive, no-symlink creation. Capacity: 64 MiB / 1024 files. No overwrite of
  workspace files. Images remain for conversation resume; capacity exhaustion
  rejects new uploads rather than silently deleting conversation attachments.
- The generated name binds credential identity, terminal and request ID. Creating
  the file durably marks an attempt before touching the terminal. Reusing that ID
  cannot submit again—even after bridge restart. Keep those files to retain this
  replay protection. There is no automatic retry of image-prompt POSTs.
- Lost acknowledgements are uncertain, not proof of failure. The app keeps the
  draft and instructs the user to inspect the terminal. Removing/reselecting an
  image creates a new request ID: do this only after resolving the prior attempt.

Successful HTTP response is `{"status":"submitted"}`: terminal input was sent,
not a promise that the selected model supports vision or accepted/completed it.
Unsupported model, account or agent errors remain visible in the terminal.

## Verification

Rust tests cover path/control rejection, image decoding, private staging,
no-overwrite/no-replay behavior, and composer guards. The ignored
`native_codex_attachment_probe` test verifies an actual image plus text in a
disposable Codex composer, never pressing Enter. Security smoke tests cover
unauthorized/revoked uploads, browser origins and the scoped larger body limit.
Android tests cover scaling limits; APK and lint are checked. Actual image-picker
and end-to-end delivery on a physical phone still require device testing.
