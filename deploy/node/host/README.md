# Host files

What a node's owner installs on the machine itself, with root:

| File | Goes to | For |
|---|---|---|
| `72-cha-virtual-pads.rules` | `/etc/udev/rules.d/` | keeps the virtual gamepads off the desktop's seat |
| `73-cha-amd-video-clocks.rules` | `/etc/udev/rules.d/` | optional; on an AMD GPU, holds the video engine's clocks at their top step (below) |
| `apparmor/cha-sandbox` | `/etc/apparmor.d/` | the profile Steam environments run under |
| `modules-load.d/cha.conf` | `/etc/modules-load.d/` | `uinput` and `uhid` at boot |

```bash
sudo deploy/node/host/install.sh     # installs what's missing or differs, reloads udev (and re-applies the rules to input, hidraw and DRM devices), loads the profile and modules
deploy/node/host/install.sh --check  # only reports; exit 1 if something would change
sudo deploy/node/host/install.sh --no-video-clocks  # everything but the AMD clocks rule
```

**AMD video clocks.** On the default `auto` power level, an amdgpu keeps the video encoder's clocks (`vclk`, `dclk`) at their lowest step under a 60 fps stream, so VA-API HEVC at 1440p took 7.05 ms a frame on a Radeon 780M. The rule sets `manual` with only those two clocks at their top step at boot, which gave 3.25 ms (p50), with no measurable power draw and the 3D and memory clocks still free to move (glmark2 9174 against 8637 on `auto`). It matches `amdgpu` devices only, so it does nothing on NVIDIA or Intel hosts. `--no-video-clocks` skips it and leaves a copy already installed alone; to undo it, delete `/etc/udev/rules.d/73-cha-amd-video-clocks.rules` and reboot. The agent treats the file as optional: absent is fine, a copy that differs from the one it carries is reported as outdated.

The agent never changes these. It reads them through read-only binds (`deploy/node/compose.yaml`), embeds the copies it was built with, and `--doctor` and its startup log say when the host's differ. Run the script again after updating the repository.
