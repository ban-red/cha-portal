# Cha Portal on Proxmox VE

There are two ways to run a Cha Node on a Proxmox VE host. Which one fits depends on the GPU.

| | An LXC container ([`create-node.sh`](create-node.sh)) | A VM with the GPU passed through |
|---|---|---|
| GPU | Intel or AMD, shared with the host and other containers | NVIDIA (or any), given to the VM alone |
| Apps | Everything, Steam included (slow on an integrated GPU) | Everything, Steam included |
| Encoding | VA-API H.264 (HEVC/AV1 where the GPU has them), or CPU H.264 | NVENC: H.264, HEVC, AV1, PyroWave |
| Setup | One script on the host | Passthrough set up on the host, then the [Quick start](../../SETUP.md#quick-start) in the VM |
| Tested | Intel UHD 630 on Proxmox 9.1: the script ran end to end with `--doctor` clean; Chrome, the test pattern and Steam's Big Picture stream from a container set up the same way. Gamepads and playing a game not yet | RTX 4090: everything |

Either can run the whole quick start (the portal and a node) or just a node for a portal you already have.

## An LXC container: `create-node.sh`

Run it as root on the Proxmox host. It needs only the script itself:

```bash
curl -fsSLO https://raw.githubusercontent.com/ban-red/cha-portal/v0.4.1/deploy/proxmox/create-node.sh
```

Read it before running it, since it runs as root on your host. Then make the quick start: the portal and a node in one container, with HTTPS through Tailscale:

```bash
sh create-node.sh --version 0.4.1 --tailscale
```

Or make a node for a portal that is already running elsewhere:

```bash
sh create-node.sh --version 0.4.1 --portal-url https://portal-host.your-tailnet.ts.net
```

`--dry-run` prints the container's settings and changes nothing. `--help` lists every option: ID, cores, memory and disk (64 GB by default; the images live there), a static `--ip`, the `--gpu` to pass in, `--data-dir` to keep app data in a host directory such as a ZFS dataset, `--join-token`, `--no-pull`, `--ssh-key`, and `--source DIR`, which copies a checkout already on the host instead of cloning the release (offline, or to try a change).

**What it does:**

1. **On the host:** loads `uinput` and `uhid` (virtual gamepads) and lists them in `/etc/modules-load.d/cha.conf` for boot. A container can't load modules itself.
2. **Creates the container:** Debian 13, privileged, with nesting on so it can run Docker. The host's Intel and AMD render nodes, each GPU's primary node (`card0`, which Steam's gamescope needs), `/dev/uinput` and `/dev/uhid` are passed in, and the input and hidraw devices the streamer makes are allowed. With `--tailscale`, `/dev/net/tun` too. The settings are appended to `/etc/pve/lxc/<id>.conf`, under a comment.
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
- **Steam works without its AppArmor profile.** Docker in a container has no AppArmor, so there is nothing for the `cha-sandbox` profile to relax, and Steam's sandboxes (bubblewrap, pressure-vessel) work because Proxmox doesn't restrict user namespaces. Its other confinement stays: no capabilities, its seccomp filter and no-new-privileges. On an Intel or AMD GPU, Steam needs a release after 0.2.0, whose agent passes the GPU's primary node to it. An integrated GPU is slow for games.
- **No NVIDIA.** The script never passes an NVIDIA GPU to a container: the container's driver would have to match the host's exactly, and CDI expects to own it. If the host has one, the script says so; give it to a VM.
- **GPU sharing.** An Intel or AMD GPU passed into a container is still the host's. Other containers, Plex for example, can use it at the same time, and they compete for its encoder.
- **Gamepads haven't been tried in a container yet.** The devices they need are passed in and allowed, but no pad has been played there yet.
- **Updating:** back up the portal's database first ([Upgrading](../../SETUP.md#upgrading-to-a-new-release)). Then in the container, `git fetch --depth 1 origin tag v0.4.1 && git checkout v0.4.1`, then `setup.sh --version 0.4.1`, then `docker compose pull && docker compose up -d` from the compose directory. For a node alone, also change the version in `deploy/node/.env` (`CHA_NODE_IMAGE`, `CHA_STREAMER_IMAGE`, `CHA_IMAGE_TAG`).
- **Removing:** `pct stop <id> && pct destroy <id>`. On the host, `/etc/modules-load.d/cha.conf` and the udev rule stay until you delete them.

### Intel GPUs: turn on the HuC

On the GPUs of 6th to 10th generation Intel Core processors (Skylake to Comet Lake and Ice Lake: HD, UHD and Iris Plus Graphics), the low-power encoder's bitrate control runs on the GPU's HuC microcontroller, and Linux doesn't load its firmware there by default. Without it, the streamer falls back to the GPU's other encoder, which borrows the shader cores the apps and the compositor also draw with. It isn't faster: on a UHD 630 at 1728×1440, H.264 took 4.6 ms a frame before and 4.9 ms after. What the low-power encoder saves is those shader cores, which matters when a game or a heavy page shares the GPU. From 11th generation Core (Tiger Lake) on, and on Arc, the kernel loads the HuC by itself. HEVC on 6th to 9th generation has no low-power encoder at all, so it stays on the slower one either way.

Check on the host. `HuC disabled` means it isn't loaded:

```bash
cat /sys/kernel/debug/dri/0/gt0/uc/huc_info
```

`create-node.sh` warns when it finds this, and `--enable-huc` sets it up. By hand, load the HuC alone (not the GuC's scheduling, which these generations don't need), rebuild the initramfs, and reboot the host:

```bash
echo "options i915 enable_guc=2" > /etc/modprobe.d/cha-i915-huc.conf
```

```bash
update-initramfs -u -k all
```

The firmware comes with Proxmox (`/lib/firmware/i915/kbl_huc_*.bin` and the like). After the reboot, `huc_info` says `status: RUNNING`, and the streamer's log says `entrypoint="EncSliceLP" rate_control="CBR"` for H.264 (checked on mars-2, a UHD 630). The reboot restarts every guest on the host, and guests not set to start on boot stay stopped, so pick its time. Check the host's clock after it too: `--doctor` compares it with the portal's, since media tokens last 60 seconds.

## A VM with the GPU passed through

This is the way to run an NVIDIA GPU on Proxmox. The VM owns the GPU, so it behaves like any other machine, and the [Quick start](../../SETUP.md#quick-start) runs in it unchanged.

1. **On the host,** turn on IOMMU and pass the GPU through, as [Proxmox's PCI passthrough guide](https://pve.proxmox.com/wiki/PCI_Passthrough) describes: `intel_iommu=on` or `amd_iommu=on` on the kernel command line, the `vfio` modules, and the GPU's driver kept off the host.
2. **Create the VM:** Ubuntu 24.04 or newer, machine type `q35`, BIOS `OVMF (UEFI)`, CPU type `host`. Give it at least 4 cores, 8 GB of memory and 100 GB of disk (more for Steam games). Add the GPU as a **PCI Device** with *All Functions* and *PCI-Express* ticked, and leave *Primary GPU* off. The streamer never needs a display.
3. **In the VM,** install the NVIDIA driver (`sudo ubuntu-drivers install`, then reboot) and check that `nvidia-smi` works. Then follow the [Quick start](../../SETUP.md#quick-start).

Gamepads need nothing more in a VM: `uinput` and `uhid` are the VM's own kernel modules, and `setup.sh` loads them.
