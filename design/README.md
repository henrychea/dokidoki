# Design source

Exported from a design tool on 2026-11-16.

| File | Role |
|---|---|
| `sumi-dark.html`, `sumi-dark.png` | Default theme. 1280×1024 logical, 2× capture. |
| `washi-light.html`, `washi-light.png` | Light theme. Same layout. |

Both HTML files are static references. They load Tailwind, Google Fonts, and Material Symbols from CDNs. Do not ship or embed them in the app.

Canonical values: the Tailwind config inside each HTML file plus the PNG. Ignore the project's "Liquid Cybernetic Glass" design metadata; it belongs to a hidden screen, and the visible screens use the Sumi/Washi palettes.

Implementation must bundle Inter and JetBrains Mono (both OFL), so the app does not depend on system fonts.
