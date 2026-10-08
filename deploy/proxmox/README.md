# Cha Portal on Proxmox VE

There are two ways to run a Cha Node on a Proxmox VE host. Which one fits depends on the GPU.

| | An LXC container ([`create-node.sh`](create-node.sh)) | A VM with the GPU passed through |
|---|---|---|
| GPU | Intel or AMD, shared with the host and other containers | NVIDIA (or any), given to the VM alone |
| Apps | Chrome, Firefox, XFCE, KDE, the test pattern | Everything, Steam included |
| Encoding | VA-API H.264 (HEVC/AV1 where the GPU has them), or CPU H.264 | NVENC: H.264, HEVC, AV1, PyroWave |
| Setup | One script on the host | Passthrough set up on the host, then the [Quick start](../../SETUP.md#quick-start) in the VM |
| Tested | Intel UHD 630 on Proxmox 9.1: the script run end to end, `--doctor` clean but for Steam's profile; Chrome and the test pattern stream from a container set up the same way. Gamepads not yet | RTX 4090: everything |

Either can run the whole quick start (the portal and a node) or just a node for a portal you already have.

## An LXC container: `create-node.sh`

Run it as root on the Proxmox host. It needs only the script itself:

```bash
curl -fsSLO https://raw.githubusercontent.com/ban-red/cha-portal/v0.2.0/deploy/proxmox/create-node.sh
```

Read it before running it, since it runs as root on your host. Then make the quick start: the portal and a node in one container, with HTTPS through Tailscale:

```bash
sh create-node.sh --version 0.2.0 --tailscale
```

Or make a node for a portal that is already running elsewhere:

```bash
sh create-node.sh --version 0.2.0 --portal-url https://portal-host.your-tailnet.ts.net
```

`--dry-run` prints the container's settings and changes nothing. `--help` lists every option: ID, cores, memory and disk (64 GB by default; the images live there), a static `--ip`, the `--gpu` to pass in, `--data-dir` to keep app data in a host directory such as a ZFS dataset, `--join-token`, `--no-pull`, `--ssh-key`, and `--source DIR`, which copies a checkout already on the host instead of cloning the release (offline, or to try a change).

**What it does:**

1. **On the host:** loads `uinput` and `uhid` (virtual gamepads) and lists them in `/etc/modules-load.d/cha.conf` for boot. A container can't load modules itself.
2. **Creates the container:** Debian 13, privileged, with nesting on so it can run Docker. The host's Intel and AMD render nodes, `/dev/uinput` and `/dev/uhid` are passed in, and the input and hidraw devices the streamer makes are allowed. With `--tailscale`, `/dev/net/tun` too. The settings are appended to `/etc/pve/lxc/<id>.conf`, under a comment.
3. **In the container:** installs git, clones the release to `/opt/cha-portal`, and runs [`deploy/quickstart/setup.sh`](../quickstart/setup.sh). That script installs Docker, writes the settings and pulls the images. It skips the host files, because inside a container they belong to the host.
4. **On the host:** installs the udev rules that keep the virtual pads off the host's own seat (`/etc/udev/rules.d/72-cha-virtual-pads.rules`), copied from that checkout. The pads appear on the host's kernel, so the host's udev is the one that sees them.
5. **Starts it:** the quick start, or with `--portal-url` the node alone, using the release's published images. A `compose.override.yaml` beside the compose file removes the NVIDIA device the agent would otherwise ask for.

It then prints what's left: give the portal HTTPS and claim it, then claim the node with the code from its log. These match steps 3–6 of the [Quick start](../../SETUP.md#quick-start), with the commands run inside the container:

```bash
pct enter <id>
```

```bash
cd /opt/cha-portal/deploy/quickstart
```

```bash
docker compose logs agent | grep unclaimed
```

For a node alone, the directory is `/opt/cha-portal/deploy/node`. Run compose from that directory, without `-f`, so it reads the `compose.override.yaml` too.

**Without Tailscale**, on a LAN where every device is yours, `--plain-http` serves the quick start over plain HTTP with sign-in still working. Read [Without HTTPS](../../SETUP.md#without-https-a-trusted-lan-only) first: the browser then gives no gamepads, no keyboard lock and no WebTransport.

**Good to know:**

- **The container is privileged.** Root in it is root on the host. A Cha Node is root-equivalent on its machine anyway, since the agent drives Docker ([SECURITY.md](../../SECURITY.md)), and Docker and the passed-in devices need privileges an unprivileged container doesn't have. Run only Cha Portal in it.
- **No Steam.** Steam's sandbox runs under an AppArmor profile, and a container can't load profiles. The portal still lists Steam, but launching it on such a node fails. Use a VM for it.
- **No NVIDIA.** The script never passes an NVIDIA GPU to a container: the container's driver would have to match the host's exactly, and CDI expects to own it. If the host has one, the script says so; give it to a VM.
- **GPU sharing.** An Intel or AMD GPU passed into a container is still the host's. Other containers, Plex for example, can use it at the same time, and they compete for its encoder.
- **Gamepads haven't been tried in a container yet.** The devices they need are passed in and allowed, but no pad has been played there yet.
- **Updating:** in the container, `git fetch --depth 1 origin tag v0.3.0 && git checkout v0.3.0`, then `setup.sh --version 0.3.0`, then `docker compose pull && docker compose up -d` from the compose directory. For a node alone, also change the version in `deploy/node/.env` (`CHA_NODE_IMAGE`, `CHA_STREAMER_IMAGE`, `CHA_IMAGE_TAG`).
- **Removing:** `pct stop <id> && pct destroy <id>`. On the host, `/etc/modules-load.d/cha.conf` and the udev rule stay until you delete them.

## A VM with the GPU passed through

This is the way to run NVIDIA and Steam on Proxmox. The VM owns the GPU, so it behaves like any other machine, and the [Quick start](../../SETUP.md#quick-start) runs in it unchanged.

1. **On the host,** turn on IOMMU and pass the GPU through, as [Proxmox's PCI passthrough guide](https://pve.proxmox.com/wiki/PCI_Passthrough) describes: `intel_iommu=on` or `amd_iommu=on` on the kernel command line, the `vfio` modules, and the GPU's driver kept off the host.
2. **Create the VM:** Ubuntu 24.04 or newer, machine type `q35`, BIOS `OVMF (UEFI)`, CPU type `host`. Give it at least 4 cores, 8 GB of memory and 100 GB of disk (more for Steam games). Add the GPU as a **PCI Device** with *All Functions* and *PCI-Express* ticked, and leave *Primary GPU* off. The streamer never needs a display.
3. **In the VM,** install the NVIDIA driver (`sudo ubuntu-drivers install`, then reboot) and check that `nvidia-smi` works. Then follow the [Quick start](../../SETUP.md#quick-start).

Gamepads need nothing more in a VM: `uinput` and `uhid` are the VM's own kernel modules, and `setup.sh` loads them.
