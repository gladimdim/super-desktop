[← Back to README](../README.md)

# How to SETUP Telegram Bot + Group

Control your SUPER DESKTOP harnesses from any Telegram client without exposing ports or trusting a third-party relay server.

## How it works (Bring Your Own Bot)

Your PC runs a local `telegram-bot` background worker that makes **outbound HTTPS long-polling requests** directly to `api.telegram.org`.
- **No third-party relay server** — only your PC and Telegram's Bot API talk to each other.
- **No open ports or port forwarding** — works behind NAT, firewalls, and on headless PCs.
- **Desktop-controlled access** — you create your own bot token, pair one Telegram account with a 6-digit desktop approval, and control every capability with per-permission switches.

---

## Step 1 — Create your bot with `@BotFather`

1. Open Telegram and search for the official **`@BotFather`** bot (with the blue verification checkmark).
2. Send `/newbot`.
3. Choose a **display name** for your bot (for example, `My Super Desktop`).
4. Choose a unique **username** ending in `bot` (for example, `my_superdesktop_bot`).
5. `@BotFather` replies with your bot token (looks like `123456789:ABC...`). Copy it and keep it private — anyone with the token can impersonate your bot.

No extra `@BotFather` configuration is required: adding the bot to groups is enabled by default, and promoting the bot to Administrator in your private Forum group lets it receive topic messages while keeping Telegram's Group Privacy Mode enabled.

---

## Step 2 — Connect and verify on your PC

1. Open **SUPER DESKTOP** (`SUPER + SHIFT + Q`) → click **⚙ Settings → Telegram**.
2. Paste your `@BotFather` token into the token field and click **Save & verify**.
   - SUPER DESKTOP checks the token directly against `api.telegram.org` and displays the bot's `@username`.
   - Use **Clear** at any time to remove a saved token.
3. Turn on **Enable Telegram bot** (nothing runs in the background until you enable it).

---

## Step 3 — Pair your Telegram account

Only **one** Telegram account can control this PC. Every pairing link is single-use and requires explicit approval on your desktop.

1. In **⚙ Settings → Telegram**, click **Pair Telegram account**.
2. Open the single-use `t.me/<bot>?start=<secret>` link in Telegram (or scan the QR code / click **Copy link**), then tap **Start** in Telegram.
3. Verify that the **6-digit code** shown in Telegram matches the pending request on your PC, then click **Approve**.
   - Clicking **Reject** blocks that Telegram user ID until you click **Clear rejected**.
4. Access is bound to your numeric Telegram `user_id`. Click **Revoke access** at any time to disconnect the paired account (revoking access also unlinks any linked Forum group).

---

## Step 4 — Set up a Forum Topics group (optional, 1 card = 1 topic)

Forum Topics turn a private 2-member Telegram supergroup into a live workspace where every running terminal card has its own topic:

1. In Telegram, create a new **private group** and add **only your bot** (there must be **2 members total**: you and your bot — SUPER DESKTOP refuses to link groups with more than 2 members for privacy).
2. Open the group's **Settings / Edit** menu and enable **Topics**.
3. Open **Administrators**, add your bot as an Administrator, and grant the **Manage Topics** permission (and optionally **Pin Messages** so the bot can silently pin the live status card at the top of each topic).
4. Open the group's **General** topic and send:
   ```text
   /linkforum
   ```
5. Your bot verifies that the chat is a Forum supergroup with `Manage Topics` enabled and at most 2 members, links the group, and syncs a color-tagged topic for each open card.

**How Forum Topics behave:**
- Messages you send inside a card's topic go straight to that terminal card (acknowledged silently with a 👍 reaction inside Forum Topics, while showing a live `"typing…"` indicator whenever the card is working).
- A **pinned live dashboard** in **General** summarizes every running card and your cached provider usage limits, with `[➕ New]`, `[🔄 Sync]`, and `[🔄 Refresh]` buttons (and `/dashboard` / `/usage` commands).
- Each topic's **pinned status header card** updates live in place (silently, with no chat spam) whenever the card's status, model, or prompt changes, and swaps its inline action buttons (for example `Yes`/`No`/`1`/`2` while `Waiting`, or `📷 Screen` / `📜 History` / `📺 Live` / `📎 Files` / `⌨ Keys` / `⏎ Enter` / `⛔ Ctrl-C` otherwise).
- When **Read terminal output** is enabled, `Waiting` alerts show the sanitized approval tail and **✅ Done** reports deliver a concise output summary right into the topic chat (preferring the agent's final response, or a cleaned terminal summary with TUI prompt boxes and toolbars stripped), and tapping **📺 Live** (or `/live`) streams a single auto-updating `<pre>` output block every ~4 seconds while the agent is active.
- Tapping **⌨ Keys** (or `/keys`) opens a quick keypad (`Yes`/`No`/`1`–`3`/`↑`/`↓`/`Tab`/`⇧Tab`/`Enter`/`Esc`/`Ctrl-C` plus harness slash shortcuts like `/resume`, `/compact`, `/clear`).
- Tapping **📎 Files** (or sending `/files`) opens a filterable, paginated list (`All` / `🖼 Images` / `📄 Docs` / `🔗 Links`) of referenced files and links (requires **Read terminal output**): images send as photos, GIFs as animations, PDFs as documents with inline **`🖼 Preview page`** buttons, long text/Markdown pages inline with **`◀ Prev` / `Next ▶` / `📥 Download file`**, and public links open from inline URL buttons.
- Topic titles stay clean (`🤖 OpenAI Codex · project — prompt`) without constant status-dot rename notices cluttering the chat; live status dots (`🟢` Working, `🟡` Waiting, `✅` Finished, `🔴` Error, `⚪` Idle, `⚫` Exited) update silently in place on each topic's pinned header card and in the **General** dashboard.
- By default, **Auto-create a topic for each card** and **Delete topic when a card closes** are enabled in **⚙ Settings → Telegram** (so new cards open topics and closed cards automatically delete their topics — falling back to archiving if Telegram denies deletion).
- Use **Sync topics now** (or `/sync` in General) to force an immediate resync of topics with open cards.
- Use **Unlink forum** in Settings (or `/unlinkforum`) to disconnect the group, or send `/unlink` inside a topic to unbind just that topic.

---

## Step 5 — Permissions & privacy

In **⚙ Settings → Telegram → Permissions**, dangerous actions default to **Off**:

| Switch | Default | Description |
| --- | --- | --- |
| **Notifications** | **On** | Push alerts when an agent finishes, waits for input, or hits an error (with inline action buttons such as `Yes` / `No` / `Ctrl-C` when control keys are enabled). |
| **Redact common secrets** | **On** | Best-effort masking of API keys, tokens, and PEM blocks before sending output to Telegram. |
| **Read terminal output** | Off | Allow `/screen` and `/history` screen captures. Output may contain sensitive text. |
| **Submit prompts** | Off | Allow sending text prompts to AI harness cards. |
| **Control keys** | Off | Allow `Yes`/`No`/`1`/`2`/`Enter`/`Esc`/`Ctrl-C` actions. |
| **Allow shell input** | Off | Allow typing text or control keys into plain Shell cards (higher risk). |
| **Launch new cards** | Off | Allow `/new` to start harnesses in remembered project folders. |
| **Close cards** | Off | Allow closing cards from Telegram after confirmation. |
| **Receive attachments** | Off | Allow photos, files, and voice clips to be staged privately for harnesses. |

> **Privacy notice:** Telegram Bot API traffic is **not** end-to-end encrypted — Telegram's servers process messages exchanged with your bot. Keep **Redact common secrets** enabled, leave unneeded permissions off, and avoid sending passwords or private keys through the chat. See [Security](../SECURITY.md) for the full threat model.

---

## Step 6 — Telegram Mini Web App (Android-like UI via Tailscale)

Want **full colored live terminal streaming**, **Weekly Usage progress bars**, **Quick Keys**, and **interactive Files & Previews** inside Telegram — without any developer relay server?

The **Telegram Mini Web App** embeds the entire web UI directly inside `super-desktop telegram-bot` on `127.0.0.1:7789`. You expose it **privately to your own Tailscale tailnet** via `tailscale serve` (which provisions a valid Let's Encrypt HTTPS certificate for `https://<pc>.<tailnet>.ts.net:8443`), and every request is authenticated locally via Telegram's `WebApp.initData` **HMAC-SHA256** signature against your paired `user_id` and bot token.

- **Zero developer relay, zero public internet exposure:** Use `tailscale serve` (private to your tailnet), never `tailscale funnel`. Terminal streams flow straight over Tailscale WireGuard between your phone and your PC.
- **No external CDN runtime code (beyond Telegram's WebApp SDK):** The UI, ANSI renderer, and WebSocket terminal client are compiled into `super-desktop`.

### Option A — Use this prompt to setup

Copy and paste the prompt below into **Claude Code, Codex, OpenCode, Antigravity, Grok, Cursor**, or any AI coding agent running on your Linux PC to configure Tailscale Serve and SUPER DESKTOP automatically (without ever printing your bot token or secrets):

```text
Configure the SUPER DESKTOP Telegram Mini Web App on this Linux PC using Tailscale Serve (never Tailscale Funnel).

Rules:
- Never print, echo, or commit my bot token (`botToken`) or any secrets from config.json.
- Use `tailscale serve` (private to my tailnet only), never `tailscale funnel`.
- Keep all existing Telegram settings (`enabled`, `botToken`, `allowedUser`, `forum`, `permissions`) intact.

Steps:
1. Check Tailscale status with `tailscale status --json` and ensure it is Running with a `Self.DNSName`.
   - If MagicDNS or HTTPS Certificates are disabled, stop and tell me what toggle to enable in https://login.tailscale.com/admin/dns.
2. Expose only SUPER DESKTOP's local loopback Mini App server privately via Tailscale Serve:
   `tailscale serve --bg --https=8443 http://127.0.0.1:7789`
   and record `URL="https://<DNSName_without_trailing_dot>:8443"`.
3. Inspect `${XDG_STATE_HOME:-$HOME/.local/state}/super-desktop/telegram/config.json`:
   - If the Telegram bot is not yet configured (`enabled: false` or empty `botToken` or no `allowedUser`), stop and guide me through Settings → Telegram (`@BotFather` token + account pairing) first.
   - Otherwise update `config.json` atomically (`chmod 600`) to set:
     `miniApp.enabled = true`, `miniApp.listenPort = 7789`, `miniApp.publicUrl = "$URL"` (preserving `miniApp.shortName` and all other fields).
4. Signal/verify the worker cleanly:
   - Check `curl -fsS http://127.0.0.1:7789/health` and `curl -fsS "$URL/health"`.
   - If the `super-desktop telegram-bot` process isn't listening on `127.0.0.1:7789` yet, wait a couple seconds after updating `config.json` (the daemon hot-reloads it) or restart SUPER DESKTOP with `super-desktop status`.
5. Report back concisely:
   - The private Mini App `$URL`
   - Whether `/health` succeeded locally and via Tailscale HTTPS
   - Next steps on my phone (keep Tailscale ON, tap ⚡ Open App in 1:1 chat with the bot or run `/app`; optionally register `/newapp` in `@BotFather` and paste the short name into Settings → Telegram → Mini Web App for Forum Topic deep links).
```

### Option B — Manual setup steps

1. **Enable MagicDNS & HTTPS Certificates** in your [Tailscale DNS settings](https://login.tailscale.com/admin/dns) and confirm Tailscale is running on both your PC and your phone.
2. **Start Tailscale Serve** on your PC (or click **Auto-setup Tailscale Serve** in **⚙ Settings → Telegram → Mini Web App**):
   ```bash
   tailscale serve --bg --https=8443 http://127.0.0.1:7789
   ```
3. **Save in SUPER DESKTOP**: open **⚙ Settings → Telegram → Mini Web App (Tailscale)**, paste `https://<your-pc>.<your-tailnet>.ts.net:8443`, turn on **Enable Telegram Mini Web App**, and click **Save Mini App**.
   - In your 1:1 chat with your bot, a **`⚡ Open App`** menu button appears automatically and `/app` launches the Mini App.
4. **Optional — One-tap buttons inside Forum Topics (`/newapp`):**
   - Message `@BotFather` → `/newapp` → select your bot → set Title, Description, Web App URL (`https://<your-pc>.<your-tailnet>.ts.net:8443`), and choose a short name (for example `superdesktop`).
   - Paste that short name into **Optional @BotFather /newapp short name** in **⚙ Settings → Telegram** and click **Save Mini App**. Every card topic and the `General` Dashboard immediately gains a **`📱 Open App`** button linking directly to that card!

---

## Bot commands reference

| Command | Action |
| --- | --- |
| `/app` | Open the Android-like Telegram Mini App (live ANSI terminal, Usage & Files) |
| `/cards` | List active terminal cards |
| `/dashboard` | Show or refresh the live workspace & usage dashboard |
| `/usage` | Show each provider's weekly allowance, progress bar, reset countdown &amp; activity |
| `/status` | Show selected card status |
| `/screen` | Capture current screen |
| `/history` | Capture recent scrollback |
| `/live` | Stream live terminal output (4 s edits while active) |
| `/keys` | Open Quick Keys & agent slash shortcuts (`↑`/`↓`/`Tab`/`⇧Tab`, `/resume`, `/compact`) |
| `/files` | Filter & browse referenced images, PDFs, text files, and links |
| `/new` | Launch a new harness card |
| `/close` | Close selected card |
| `/ctrlc` | Send Ctrl-C to selected card |
| `/enter` | Send Enter to selected card |
| `/esc` | Send Escape to selected card |
| `/sync` | Sync Forum Topics with cards |
| `/linkforum` | Link a 2-member Forum group |
| `/unlinkforum` | Unlink Forum group |
| `/unlink` | Unlink this Forum Topic from its card |
| `/help` | Show help and active permissions |

---

## Troubleshooting

| Symptom | Fix |
| --- | --- |
| Worker chip shows `Stopped`, `Starting`, or `Error` | Check **⚙ Settings → Telegram**. Make sure the token is saved with **Save & verify**, **Enable Telegram bot** is on, and your PC can reach `api.telegram.org`. |
| `Refused: this group has N members...` | Remove every member except you and your bot so the group has at most 2 members, then run `/linkforum` again. |
| `Promote this bot to Administrator with the "Manage Topics" permission first...` | In Group Settings → Administrators, promote your bot to Admin and turn on **Manage Topics**. |
| `This chat is not a Forum Supergroup...` | Enable **Topics** in Group Settings first, then send `/linkforum` in **General**. |
| Bot replies that an action is disabled | Turn on the matching permission switch in **⚙ Settings → Telegram → Permissions** (for example, `Read terminal output`, `Submit prompts`, `Control keys`, or `Receive attachments`). |
| Messages sent while PC was asleep/offline do nothing | For safety, stale messages older than ~2 minutes (or queued before the worker started) are ignored so old commands never replay unexpectedly on wake. Send a fresh message or `/cards`. |
