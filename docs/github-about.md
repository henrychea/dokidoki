# GitHub repository settings

Copy these values into the repository settings after the code is published.

## About

**Description** (306 characters, limit 350):

> dokidoki is a local-first desktop app and CLI that auto-commits changed git repositories with AI-written messages (Claude Code, Codex, OpenCode, omp, pi) and safely pushes them on a schedule. It never force-pushes, scans for secrets before any model call, and supports dry run and undo. Rust + GPUI, Linux.

**Website**:

> `https://henrychea.github.io/dokidoki/`

## Topics

Twenty lowercase, hyphenated topics:

```text
git
auto-commit
git-sync
ai-commit
commit-message-generator
gpui
rust
desktop-app
linux
developer-tools
backup
claude-code
codex
llm
automation
git-automation
local-first
systemd
kde
wayland
```

## Social preview

Use `docs/screenshots/sumi.png` as the source image. It is 1280×1024; GitHub
shows a 1280×640 preview, so crop to the center of the window.

With ImageMagick 7 (`convert` on ImageMagick 6):

```sh
magick docs/screenshots/sumi.png \
  -resize '1280x640^' -gravity center -extent 1280x640 \
  docs/screenshots/social-preview.png
```

`-gravity north` pins the header instead of centering, which keeps the wordmark
and repository tabs; `center` keeps the repository header and commit studio.
Either is acceptable. Keep the result a PNG or JPG under 1 MB, then upload it
under **Settings → Social preview**.
