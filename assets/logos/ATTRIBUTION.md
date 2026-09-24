# Harness logo sources

Downloaded/verified 2026-09-24. Bundled locally for Linux and Android. Product
marks identify their respective harnesses; trademarks remain with their owners.
No affiliation or endorsement is implied.

The manifest records source URLs and hashes. Artwork geometry and brand colors
are retained; animations are removed for static controls. Aider/Goose get white
foreground variants on dark themes; Kimi uses its official variants.

| Harness | SVG source |
| --- | --- |
| claude | https://claude.ai/favicon.svg  |
| pi | https://pi.dev/logo.svg  |
| openclaw | https://raw.githubusercontent.com/openclaw/openclaw/main/ui/public/favicon.svg  |
| hermes | https://web-assets.nousresearch.com/nousnet-web/assets/hermes-landing/teams/hermes-wing.6ee276e9bff5a166.svg Official Hermes wing mark from the current Nous Research landing page. |
| goose | https://raw.githubusercontent.com/block/goose/main/documentation/static/img/goose.svg  |
| qwen | https://raw.githubusercontent.com/QwenLM/qwen-code/main/packages/desktop-shell/bootstrap/qwen-code-logo.svg  |
| crush | https://raw.githubusercontent.com/charmbracelet/crush/main/internal/oauth/callback/heartbit.svg  |
| aider | https://raw.githubusercontent.com/Aider-AI/aider/main/aider/website/assets/icons/safari-pinned-tab.svg  |
| kiro | https://kiro.dev/icon.svg  |
| cursor | https://cursor.com/marketing-static/favicon.svg  |
| reasonix | https://raw.githubusercontent.com/esengine/reasonix/HEAD/desktop/frontend/src/assets/logo-symbol.svg  |
| gemini | https://raw.githubusercontent.com/lobehub/lobe-icons/master/packages/static-svg/icons/gemini-color.svg Google Gemini mark from Lobe Icons (MIT). |
| antigravity | https://raw.githubusercontent.com/sst/opencode/dev/packages/ui/src/assets/icons/app/antigravity.svg Product mark from OpenCode integration assets; Google site SVG embeds a raster. |
| kimi | https://moonshotai.github.io/Branding-Guide/ Official K-only light/dark SVG variants. |
| t3code | https://raw.githubusercontent.com/pingdotgg/t3code/main/assets/prod/logo.svg  |
| codex | https://commons.wikimedia.org/wiki/File:OpenAI_logo_2025_(symbol).svg  |
| opencode | https://github.com/sst/opencode/tree/dev/packages/console/app/src/asset/brand  |
| grok | https://commons.wikimedia.org/wiki/File:Grok-feb-2025-logo.svg  |
| shell | SUPER DESKTOP generic terminal glyph (not a product logo)  |

Herder publishes no verifiable SVG logo in its upstream repository
(https://github.com/cleonhp88/herder); it retains its existing fallback. Custom
harnesses keep user-selected icons. Older Anthropic/Google assets are retained
for compatibility but Claude and Antigravity now use their product marks.

Run `python3 scripts/sync-harness-logos.py` to mirror these same SVGs and manifest
to Android. `--check` checks hashes and repository parity without writing files.

Grok uses the two standalone symbol paths from the sourced wordmark, with a square viewBox; lettering is omitted for small icons.

Antigravity’s opaque alpha mask is expressed as an equivalent white luminance mask for AndroidSVG; visible artwork is unchanged.

AndroidSVG does not implement SVG blur filters. Antigravity therefore uses a
256px raster rendered from the exact bundled SVG by librsvg, preserving its
gradients rather than dropping its filters. The SVG remains the source asset.
Regenerate with `rsvg-convert -w 256 -h 256 -a antigravity.svg -o antigravity-android.png`.

## Themeable variants

`*-themed.svg` files are derived from the sourced light-background marks by
`scripts/theme-harness-logos.py`. Visible paints become two palette roles: theme
accent (`#123456` placeholder) and theme background (`#fedcba` placeholder).
Geometry, proportions, clipping and luminance-mask colors are retained. Color
blend filters are omitted for these monochrome marks. Both apps substitute the
active palette at render time; originals are retained as source/fallback assets.

Theme-specific interior roles preserve OpenClaw/Crush eyes and OpenCode screen
shading. Antigravity's monochrome variant uses the exact upstream silhouette
mask path directly, removing the need for overlapping blurred color layers.
