# Android completion alerts

Android's terminal header has a per-terminal bell. Enable it to receive
`Codex ★ Chumaki finished` (agent + project-folder name) in a tappable in-app
banner and in Android's notification tray. In-app delivery silences the tray
notification. The public lock-screen version hides the project name. Prompts and
responses are never included. Settings are local to each phone, scoped to the
bridge certificate fingerprint and tmux terminal ID.

## Detection and compatibility

The first adapter supports Codex CLI sessions with an attributable open rollout
file. No WORKING/IDLE heuristic, screen text, CPU activity or output timeout is
used as completion evidence. The bridge follows the exact tmux pane process to
the nearest Codex process, then its open rollout descriptor. It does not search
for the newest conversation in the workspace or modify user Codex configuration.
Ambiguous/multi-pane sessions, subagent rollouts, and unsupported formats fail
closed. Other harnesses explain that reliable alerts are not yet supported.

An explicit `event_msg/task_complete` with a turn ID and nonempty final assistant
message marks a completed response. Later starts, user messages, aborts and
errors clear it. Tool/item completion and intermediate agent messages do not
qualify. A partial final record or a file changing during the read suppresses
the result until a later check. IDs are stable hashes of rollout identity + turn
ID, not message text. This is **turn completion**, not a guarantee that a larger
autonomous goal or a background task has finished. Very rapid subsequent turns
can suppress an earlier completion before it is observed.

Codex's documented `notify` hook confirms the distinction between turn completion
and approval requests, but this adapter reads the locally observed rollout
format (`source: cli`, `task_started/task_complete`) instead of overriding that
hook. Rollout format is internal, version-sensitive, and not a stable public API.
When it cannot be verified, the adapter does not invent a completion event.

Source: https://learn.chatgpt.com/docs/config-file/config-advanced#notifications

## Delivery and limits

- Android takes a baseline before enabling a bell: no alert for an existing old
  completion. Last-delivered IDs survive restart/reconnect. Repeated snapshots
  do not alert again. A crash between posting and persisting may repeat an alert;
  the same terminal notification replaces its prior tray entry.
- Monitoring polls only selected terminals, batched per bridge, every five
  seconds while runnable, with three concurrent bridge requests and up to a
  minute of retry backoff. No terminal contents or WebSocket frames are polled.
- Up to 32 terminals per phone. Each changed rollout read is bounded to a 64 KiB
  metadata header and 512 KiB tail, cached by file metadata (64 entries).
  Very large individual events can prevent detection rather than cause a guess.
- The opt-in `connectedDevice` foreground service has a persistent, low-priority
  monitor notification with a **Turn off all bells** action. It continues when
  the activity is hidden or dismissed from Recents, subject to Android/OEM policy.
- No Firebase, relay, wake lock, or battery-optimization exemption. Network loss,
  Doze, Samsung battery restrictions, and OS process termination can delay alerts.
  Force-stop cannot be bypassed. After reboot/force-stop, reopen the app to resume.
  A reachable bridge is required; use an appropriate network connection remotely.
- Disconnects do not manufacture completion events. On reconnect, the latest
  still-completed response may be delivered, not a replay of every missed turn.
- Turning off the final bell stops the service. Removing a saved bridge removes
  its watches on the next monitor pass. Expired/revoked credentials cannot fetch
  completion metadata. A closed terminal cannot report completion; use the
  monitor's Stop action to clear any obsolete watch.

Android requirements:
https://developer.android.com/develop/background-work/services/fgs/service-types#connected-device

## Authenticated API

`POST /api/v1/completions`, same pinned TLS + bearer authorization as the rest of
the bridge (including localhost). Request: `{"sessions":["sd_term_..."]}`.
Maximum 32 identifiers, each 1–128 ASCII letters/digits/underscores/hyphens.

Response: `{"terminals":[{"id":"sd_term_...","supported":true,"state":"completed","completionId":"64-character lowercase SHA-256"}]}`.
States are `working`, `completed`, or `unknown`. Missing/non-Codex/unattributable
sessions return `supported:false`, `state:unknown`, `completionId:null`.
The endpoint exposes no process IDs, filesystem paths, prompts, or responses.
It uses the existing four-job admission cap and bridge revocation enforcement.

## Verification / device checklist

Unit tests cover explicit vs intermediate events, aborts/new turns, incomplete
records, stable IDs, and process-owned descriptor attribution/cache refresh using
an isolated fixture process. Wire tests cover missing authorization, revoked
tokens, malformed IDs and oversized lists. Android tests cover completion
deduplication and notification wording.

Before declaring phone delivery verified, install both updated builds and test:
bell OFF/ON; old completion suppression; completed prompt; tool wait/approval;
interrupt; two terminals in the same folder; foreground banner and tap;
background/Recents dismissal; screen-off and Samsung battery restrictions;
denied permission/channel; bridge disconnect/reconnect/revocation; process
restart; disabling the last bell; removing the bridge. No phone was connected
during initial implementation, so device delivery is not yet verified.
