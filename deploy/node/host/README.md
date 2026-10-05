# Host files

What a node's owner installs on the machine itself, with root:

| File | Goes to | For |
|---|---|---|
| `72-cha-virtual-pads.rules` | `/etc/udev/rules.d/` | keeps the virtual gamepads off the desktop's seat |
| `apparmor/cha-sandbox` | `/etc/apparmor.d/` | the profile Steam environments run under |
| `modules-load.d/cha.conf` | `/etc/modules-load.d/` | `uinput` and `uhid` at boot |

```bash
sudo deploy/node/host/install.sh     # installs what's missing or differs, reloads udev, loads the profile and modules
deploy/node/host/install.sh --check  # only reports; exit 1 if something would change
```

The agent never changes these. It reads them through read-only binds (`deploy/node/compose.yaml`), embeds the copies it was built with, and `--doctor` and its startup log say when the host's differ. Run the script again after updating the repository.
