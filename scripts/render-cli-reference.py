#!/usr/bin/env python3
"""Render CLI.md for GitHub Pages; requires Python-Markdown (python-markdown).

Run after editing CLI.md: python3 scripts/render-cli-reference.py
Use --check to verify that the checked-in HTML matches its source.
Verify the command table against `super-desktop schema --format json` when
changing public commands. This script never contacts or changes the daemon.
"""
import argparse
from pathlib import Path

import markdown

ROOT = Path(__file__).resolve().parents[1]
GITHUB = "https://github.com/gladimdim/super-desktop/blob/master/"


def render():
    converter = markdown.Markdown(extensions=["fenced_code", "tables", "toc"])
    body = converter.convert((ROOT / "CLI.md").read_text())
    for filename in ("README.md", "SECURITY.md"):
        body = body.replace(f'href="{filename}', f'href="{GITHUB}{filename}')
    body = body.replace('<table>', '<div class="table-scroll" tabindex="0" role="region" aria-label="Reference table"><table>')
    body = body.replace('</table>', '</table></div>')
    return '''<!DOCTYPE html>
<!-- Generated from CLI.md by scripts/render-cli-reference.py. Edit the Markdown source. -->
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>SUPER DESKTOP CLI reference for people and AI agents</title>
<meta name="description" content="Commands, JSON results, launch permissions and request receipts for controlling SUPER DESKTOP from scripts and AI agents.">
<link rel="canonical" href="https://superdesktop.dmytrogladkyi.com/cli.html">
<meta property="og:type" content="website">
<meta property="og:site_name" content="SUPER DESKTOP">
<meta property="og:locale" content="en_US">
<meta property="og:url" content="https://superdesktop.dmytrogladkyi.com/cli.html">
<meta property="og:title" content="SUPER DESKTOP CLI reference">
<meta property="og:description" content="Commands and JSON results for driving SUPER DESKTOP from scripts and AI agents.">
<meta property="og:image" content="https://superdesktop.dmytrogladkyi.com/media/social-card.jpg">
<meta property="og:image:type" content="image/jpeg">
<meta property="og:image:width" content="1200">
<meta property="og:image:height" content="630">
<meta property="og:image:alt" content="SUPER DESKTOP: every AI agent, one shortcut away. The overlay with live Claude Code and Codex terminals.">
<meta name="twitter:card" content="summary_large_image">
<meta name="twitter:title" content="SUPER DESKTOP CLI reference">
<meta name="twitter:description" content="Commands and JSON results for driving SUPER DESKTOP from scripts and AI agents.">
<meta name="twitter:image" content="https://superdesktop.dmytrogladkyi.com/media/social-card.jpg">
<meta name="twitter:image:alt" content="SUPER DESKTOP: every AI agent, one shortcut away. The overlay with live Claude Code and Codex terminals.">
<meta name="theme-color" content="#050505">
<link rel="icon" href="favicon.ico">
<link rel="stylesheet" href="assets/terminal.css">
<script src="assets/theme-toggle.js"></script>
<style>
  .contents { margin-bottom: 18px; padding: 12px 16px; background: var(--box-bg); border: 1px solid var(--line); }
  .contents summary { cursor: pointer; font-weight: 700; color: var(--lime); }
  .contents ul { padding-left: 20px; margin: 8px 0 0; columns: 2; column-gap: 24px; }
  .contents li { margin-bottom: 4px; font-size: 12.5px; color: var(--muted); }
  @media (max-width: 720px) { .contents ul { columns: 1; } }
  main h2 { margin: 34px 0 12px; padding-top: 14px; border-top: 2px solid var(--line); color: var(--lime); font-size: 20px; scroll-margin-top: 64px; }
  main p, main li { overflow-wrap: anywhere; color: var(--muted); }
  main li strong, main p strong { color: var(--text); }
  th, td { min-width: 110px; }
</style>
</head>
<body>
<header class="wmbar">
  <div class="wrap">
    <a class="wm-tag" href="index.html">[← SUPER DESKTOP]</a>
    <span class="wm-path">~/cli</span>
    <nav class="wm-links" aria-label="Main navigation">
      <a href="https://github.com/gladimdim/super-desktop/blob/master/CLI.md">CLI.md on GitHub</a>
      <a href="https://raw.githubusercontent.com/gladimdim/super-desktop/master/CLI.md">Raw Markdown for agents</a>
    </nav>
    <button type="button" class="mode-toggle" id="mode-toggle" aria-label="Switch color theme" title="Switch white / dark terminal theme">☀️</button>
  </div>
</header>
<div class="wrap doc">
  <section class="pane focus">
    <div class="pane-bar"><span class="path">[00] ~/cli/reference</span><span class="badge">CLI · JSON · STDIO MCP (88 TOOLS)</span></div>
    <div class="pane-body">
<details class="contents"><summary>On this page</summary><nav aria-label="Contents">''' + converter.toc + '''</nav></details>
<main>''' + body + '''</main>
    </div>
  </section>
<footer><div class="wrap"><span>Command availability depends on your installed client and running daemon. Start with <code>super-desktop help agents</code>.</span><nav aria-label="Footer"><a href="index.html">home</a><a href="https://github.com/gladimdim/super-desktop">github</a></nav></div></footer>
</div></body>
</html>
'''


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    destination = ROOT / "docs" / "cli.html"
    result = render()
    if args.check:
        if not destination.exists() or destination.read_text() != result:
            raise SystemExit("CLI HTML is stale; run python3 scripts/render-cli-reference.py")
        print("CLI HTML matches CLI.md")
    else:
        destination.write_text(result)
        print("Rendered docs/cli.html")
