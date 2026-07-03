# Deploying TEMM1E as an autonomous desktop agent (headed cloud VPS)

Goal: temm1e boots with the machine, comes up **inside a graphical session**, and drives
the desktop natively (mouse + keyboard + vision) **with no human in the loop** — the
"acts on the user's behalf, unattended" case.

The one thing that makes or breaks this is the **input backend**, because Wayland gates
synthetic input behind an interactive permission prompt. On a host *you* provision you
simply choose an environment that doesn't prompt.

## Backend choice (pick the row for your session)

| Session on the VPS | `TEMM1E_INPUT_BACKEND` | Prompt? | Click accuracy | Setup |
|---|---|---|---|---|
| **X11** (GNOME-on-Xorg, XFCE, MATE…) | *(unset → `auto`)* | **none** | **exact** | none — enigo/XTEST just works |
| **Wayland** | `ydotool` | **none** | exact keys; clicks imprecise¹ | ydotoold + /dev/uinput (below) |
| **Wayland** (attended only) | *(unset → `auto`)* | one per launch | exact | libei portal (needs a human to approve) |

¹ ydotool absolute motion is distorted by pointer acceleration; keyboard is exact. Prefer **X11** on a VPS to avoid this entirely. (Precise Wayland clicking without a prompt is tracked as follow-up: a diff-centroid correction loop, or GNOME `grdctl` headless RDP.)

> **Recommendation: run an X11 desktop on the VPS.** temm1e already selects enigo/XTEST on
> X11 — exact, promptless, zero backend config. Wayland is the only environment that needs
> the `ydotool` override, and even then clicking is the weak spot.

## Recommended path — X11 VPS

1. **Provision a headed VPS** with a desktop environment (e.g. Ubuntu Desktop, or install
   one: `sudo apt install ubuntu-desktop-minimal`). You'll view/attach via the provider's
   console, VNC, or RDP.

2. **Use an X11 session.** At the login screen pick "GNOME on Xorg" (gear icon), or install
   an X11 DE like XFCE. Verify inside the session:
   ```bash
   echo "$XDG_SESSION_TYPE"   # must print: x11
   ```

3. **Enable auto-login** for the agent user so the desktop starts at boot with no password.
   GNOME/GDM — edit `/etc/gdm3/custom.conf`:
   ```ini
   [daemon]
   AutomaticLoginEnable=true
   AutomaticLogin=agent      # your agent user
   WaylandEnable=false       # force X11 for the greeter/session
   ```
   (LightDM: set `autologin-user=agent` in `/etc/lightdm/lightdm.conf`.)

4. **Build + install temm1e** with desktop control:
   ```bash
   cargo build --release --bin temm1e --features desktop-control
   install -Dm755 target/release/temm1e ~/.local/bin/temm1e
   ```

5. **Autostart it in the session** — either:
   - **systemd user unit** (recommended): `deploy/temm1e-desktop.service` →
     `~/.config/systemd/user/`, then `systemctl --user enable --now temm1e-desktop.service`; or
   - **XDG autostart**: `deploy/temm1e-desktop.desktop` → `~/.config/autostart/` (edit `Exec`).

6. **Reboot and verify:** the VPS boots → auto-login → X11 desktop → temm1e starts inside it
   and can click/type immediately, no prompt. Check logs:
   ```bash
   systemctl --user status temm1e-desktop.service
   journalctl --user -u temm1e-desktop.service -f
   ```

## Wayland VPS variant (if you can't use X11)

Everything above, except: force the promptless backend and run the uinput daemon.

1. In the unit/autostart, set `TEMM1E_INPUT_BACKEND=ydotool` (uncomment it in
   `deploy/temm1e-desktop.service`, or use the `env …` form in the `.desktop`).
2. Install ydotool and start its daemon at boot with /dev/uinput access:
   ```bash
   sudo apt install ydotool
   sudo systemctl enable --now ydotool     # provides ydotoold on the uinput device
   # (or a udev rule granting your user rw on /dev/uinput + a user ydotoold service)
   ```
3. Keyboard is exact; **clicks are imprecise** on Wayland/ydotool — plan around it (keyboard
   navigation, the browser/Prowl tool for web, or wait for the click-correction follow-up).

`auto`/`libei` is **not** appropriate here — `LibeiController::new()` blocks on the
RemoteDesktop portal dialog, and there is no one to approve it on an unattended host.

## How the knob works

`DesktopController` reads `TEMM1E_INPUT_BACKEND` (`auto` | `enigo` | `ydotool` | `libei`) via
`InputBackendPref::from_env()`. Only `auto` (on Wayland) and `libei` ever attempt the portal;
`enigo`/`ydotool` skip it entirely, so an unattended host never hangs on the dialog. On X11,
`auto` already resolves to enigo/XTEST, so no configuration is needed.

## Security note

Auto-login + an agent that controls the desktop means the VPS *is* the trust boundary: treat
it as a dedicated, isolated agent box (own user, minimal other software, firewalled). This is
the same posture as any cloud "computer-use" agent VM. The Wayland portal prompt exists
precisely to stop *arbitrary* processes from doing this silently — bypassing it (X11/ydotool)
is a deliberate, one-time, host-owner decision, not something granted to random code.
