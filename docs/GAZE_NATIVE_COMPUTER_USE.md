# Tem Gaze — Native Computer Use (North Star)

**Date:** 2026-07-03
**Status:** Direction agreed; partially implemented (branch `feat/gaze-som-grid-airtight-loop`)
**Owner:** gaze (`crates/temm1e-gaze`) + desktop tool (`crates/temm1e-tools/src/desktop_tool.rs`)

---

## Vision

temm1e should control the computer **natively, driven by a vision model** — it looks at the
screen and acts like a real user navigating this PC and its apps: moving the mouse, clicking,
typing, reading the result, correcting. Not a protocol talking to one app — a user operating the
whole machine.

## Principle: native OS input, not automation protocols

We already have **Tem Prowl** (browser via CDP) for web. It is deliberately **not** the primary
path for this vision, because CDP/automation is *detectable*: `navigator.webdriver`, the CDP
runtime, and automation flags are exactly what anti-bot stacks (Cloudflare, DataDome, reCAPTCHA,
PerimeterX) fingerprint and challenge. That is the wall Prowl keeps hitting.

**Native input into a normally-launched real app has no automation channel to detect.** A real
browser driven by real mouse/keyboard events is being *used*, not *automated* — so it sidesteps
the entire class of CDP-fingerprint defenses, and it generalizes past the browser to any native
application. This is the core reason native computer-use is stronger than Prowl for hostile
targets.

Prowl is not deprecated — for cooperative web tasks it is faster and more precise. The routing
rule is: **native gaze for anything that must look human or isn't a browser; Prowl for
cooperative web automation.**

## Architecture

Two layers, cleanly separated:

```
        ┌─────────────────────────────────────────────┐
        │  Vision + verification loop  (platform-      │
        │  agnostic; already built)                    │
        │   capture → SoM grid → VLM → action →        │
        │   post-capture → frame-diff → verify         │
        └───────────────────────┬─────────────────────┘
                                │  (x, y) / text / key
        ┌───────────────────────▼─────────────────────┐
        │  InputBackend  (one trait, best native path  │
        │  per platform, runtime-detected, honest)     │
        │   enigo(X11/macOS/Windows) · libei(Wayland)  │
        │   · ydotool(Wayland fallback)                │
        └──────────────────────────────────────────────┘
```

- **Vision + verify layer** — the value that makes native input *safe and correcting*: the
  Set-of-Mark coordinate grid (so the VLM targets by readable coordinates, not raw-pixel guessing)
  and objective frame-change detection (so a miss is *measured*, not confabulated). This is
  platform-agnostic and does not care which backend delivers the click.
- **InputBackend** — pick the best *native* injection API for the host at runtime; report
  capability honestly; never fabricate success. (Today: `InputRoute` in
  `crates/temm1e-gaze/src/input.rs`.)

## Platform coverage — native input is easy on most real machines

The hard part is one platform, not "computer use" in general. We debugged on the worst case first.

| Platform | Native API | Fingerprint | Status |
|---|---|---|---|
| **Windows** (majority of desktop users) | `SendInput` (enigo) | none at input layer | ✅ works today via enigo |
| **macOS** | `CGEvent` (enigo) | none; needs one-time Accessibility grant | ✅ works; permission UX to polish |
| **Linux X11** | `XTEST` (enigo) | none for normal apps | ✅ works; HiDPI scale fix landed |
| **Linux Wayland** | libei / RemoteDesktop portal | none | ⚠️ the one hard slice — see #70 |

On Windows/macOS/X11, native absolute input is a single reliable API call — undetectable and
exact. Wayland deliberately removed global input injection for security, which is the *only*
reason this is hard there.

## Current state (branch `feat/gaze-som-grid-airtight-loop`)

Built and verified on real hardware (GNOME Wayland, 1920×1200, scale 1.0 — the hard platform):

- **Grid Set-of-Mark** (`overlay.rs::overlay_coordinate_grid`) — labelled magenta coordinate grid
  in LOGICAL px, HiDPI-correct, on every screenshot. Live-tested: readable over real UI.
- **Airtight verify loop** (`desktop_tool.rs::act_and_verify` + `diff.rs::compare_frames`) — every
  mutating action captures pre → acts → captures post → diffs, and reports objective % change
  (globally + in a region around the action). Live-tested: static baseline ≈0 (no false positive);
  a real click's miss surfaced as `region_ratio = 0.0` instead of a false "done".
- **enigo coordinate contract** — logical→physical scaling for enigo/X11 HiDPI (`scale_point` /
  `enigo_coords`); no-op at scale 1.0 and on macOS/Windows.
- **Honest availability** — ydotoold-liveness hint in the input status.
- **Live self-test harness** (`examples/gaze_selftest.rs`) — reproducible capture/grid/diff/input
  proof on a real display.

**Empirically confirmed working on Wayland:** screen capture, grid, change-detection, and
**keyboard** input (`type`/`key`). **Empirically broken on Wayland:** mouse-click *positioning*
via ydotool — commanded (400,300) landed at ≈(1410,600) due to libinput acceleration on ydotool's
virtual relative device (deterministic; ydotool's own docs warn to disable acceleration). This is
the entire justification for the libei backend.

## Roadmap (open work)

1. **libei / RemoteDesktop input backend for Wayland (#70)** — ✅ **DONE (PR #71).** `LibeiController`
   via ashpd's direct-portal `notify_pointer_motion_absolute` (no reis/EIS needed); exact logical
   coordinates, no acceleration; one permission prompt per process, then reused. Proven on real
   GNOME Wayland (click hit calc "5"; typed "TEMM1E gaze libei 789"). Preferred over ydotool on
   Wayland unless `TEMM1E_INPUT_BACKEND` overrides (see Autonomy below).
2. **macOS Accessibility + Windows packaging** — permission grant flow and signed distribution;
   UX, not hard problems.
3. **Vision layer polish** — optional diff-centroid servo (localize where a click *actually* landed
   and re-aim) as a backend-agnostic accuracy aid — also the promptless-Wayland click-precision fix;
   element/accessibility-tree targeting where a native a11y API exists.

## Autonomy & deployment (attended vs unattended)

Wayland gates synthetic input behind a portal consent dialog, so the exact-but-interactive libei
backend prompts **once per process**. Fine for an attended personal desktop; but a truly
unattended host (a headed cloud VPS) has no one to approve it. The backend is therefore selectable
via `TEMM1E_INPUT_BACKEND` (`auto` | `enigo` | `ydotool` | `libei`, read by
`InputBackendPref::from_env()`):

- **X11 host** (incl. an X11 VPS) — `auto` → enigo/XTEST: exact **and** promptless. No config. Recommended for a VPS.
- **Unattended Wayland** — `ydotool`: promptless (one-time `/dev/uinput` setup), keyboard exact, clicks imprecise.
- **Attended Wayland** — `auto`/`libei`: exact, one approval per launch.

`enigo`/`ydotool` never attempt libei, so an unattended host never blocks on the dialog. Full
boot-to-running-agent setup (auto-login + autostart) is in `docs/DEPLOY_AUTONOMOUS_DESKTOP.md`,
with units in `deploy/temm1e-desktop.{service,desktop}`.

## Honest limits — input is necessary, not the whole evasion story

Native input removes the **#1** anti-bot tell (the automation channel). A determined target also
inspects browser fingerprint (canvas/WebGL/UA), IP reputation, and **behavioral** signals
(mouse-path smoothness, timing). Full "real user" fidelity is native input **+** a real browser
profile **+** residential IP **+** human-like motion/timing. Gaze's screenshot-paced, vision-driven
actions already look more human than CDP's instant DOM pokes — but input alone is the foundation,
not the entire picture. Track behavioral realism as a separate line item.

## References
- Issues: [#70](https://github.com/temm1e-labs/temm1e/issues/70) (Wayland native input),
  [#69](https://github.com/temm1e-labs/temm1e/issues/69) (unrelated: history-scoping confabulation)
- Code: `crates/temm1e-gaze/{overlay,diff,input,desktop_controller}.rs`,
  `crates/temm1e-tools/src/desktop_tool.rs`, `crates/temm1e-gaze/examples/gaze_selftest.rs`
- Prior art (native-input-on-Wayland): libei (Peter Hutterer), `reis`, GNOME Remote Desktop, input-leap
