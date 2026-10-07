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
<style>
  :root { color-scheme: dark; --bg: #0b0e14; --panel: #131722; --text: #e6e9f0; --muted: #a8b2c5; --accent: #22d3ee; --border: #262e42; }
  * { box-sizing: border-box; }
  body { margin: 0; background: var(--bg); color: var(--text); font: 17px/1.7 system-ui, sans-serif; }
  .wrap { max-width: 1040px; margin: auto; padding: 24px 24px 72px; }
  .links { display: flex; flex-wrap: wrap; gap: 12px 24px; font-size: 15px; }
  a { color: var(--accent); text-underline-offset: 3px; overflow-wrap: anywhere; }
  a:focus-visible, [tabindex]:focus-visible { outline: 3px solid var(--accent); outline-offset: 4px; }
  h1 { font-size: clamp(28px, 5vw, 42px); line-height: 1.2; margin-top: 36px; }
  h2 { margin: 48px 0 16px; line-height: 1.3; scroll-margin-top: 20px; }
  p, li { overflow-wrap: anywhere; }
  code { font: .88em ui-monospace, SFMono-Regular, Consolas, monospace; overflow-wrap: anywhere; }
  pre { padding: 18px; background: var(--panel); border: 1px solid var(--border); border-radius: 12px; overflow-x: auto; }
  pre code { white-space: pre; overflow-wrap: normal; }
  .contents { margin-top: 28px; padding: 16px 24px; background: var(--panel); border: 1px solid var(--border); border-radius: 12px; }
  .contents summary { cursor: pointer; font-weight: 650; }
  .contents ul { padding-left: 20px; }
  .table-scroll { overflow-x: auto; margin: 24px 0; }
  table { width: 100%; border-collapse: collapse; font-size: 15px; }
  th, td { padding: 12px; text-align: left; vertical-align: top; border: 1px solid var(--border); min-width: 110px; }
  th { background: var(--panel); }
  footer { margin-top: 48px; color: var(--muted); font-size: 14px; }
  @media (max-width: 600px) { .wrap { padding: 20px 16px 48px; } body { font-size: 16px; } pre { padding: 12px; } }
  @media (pointer: coarse) { .links a, .contents summary { display: inline-flex; align-items: center; min-height: 44px; } .contents li { margin: 6px 0; } }
</style>
</head>
<body><div class="wrap">
<nav class="links" aria-label="Main navigation">
  <a href="index.html">← SUPER DESKTOP</a>
  <a href="https://github.com/gladimdim/super-desktop/blob/master/CLI.md">Reference on GitHub</a>
  <a href="https://raw.githubusercontent.com/gladimdim/super-desktop/master/CLI.md">Plain Markdown for agents</a>
</nav>
<details class="contents"><summary>On this page</summary><nav aria-label="Contents">''' + converter.toc + '''</nav></details>
<main>''' + body + '''</main>
<footer>Command availability depends on your installed client and running daemon. Start with <code>super-desktop help agents</code>.</footer>
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
