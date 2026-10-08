import type { Inventory } from "./api";
import { gigabytes } from "./format";

type Platform = NonNullable<Inventory["platform"]>;
type Disk = NonNullable<Inventory["disks"]>[number];

/** Free space below this reads as low. */
export const LOW_DISK_BYTES = 10 * 2 ** 30;

/** "Debian 13 in an LXC container": the OS and what it runs on; just the OS when the platform is unknown. */
export function platformLine(os: string, platform: Platform): string {
  const detail = platform.detail?.trim();
  switch (platform.kind) {
    case "bare-metal":
      return `${os}, bare metal`;
    case "vm":
      if (!detail) return `${os} in a VM`;
      return /\bVM\b/.test(detail) ? `${os} in a ${detail}` : `${os} in a VM (${detail})`;
    case "lxc":
      return `${os} in an LXC container`;
    case "wsl":
      return `${os} on Windows (WSL 2)`;
    case "docker-desktop":
      return `${os} in Docker Desktop`;
    default:
      return os;
  }
}

export function diskLabel(uses: Disk["uses"]): string {
  const images = uses.includes("images");
  const appData = uses.includes("appData");
  if (images && appData) return "Images and app data";
  if (images) return "Images";
  if (appData) return "App data";
  return "Disk";
}

export interface DiskView {
  label: string;
  /** "120 GB of 500 GB free" */
  text: string;
  /** Share used, 0-100. */
  usedPct: number;
  low: boolean;
  path: string;
}

export function diskView(disk: Disk): DiskView {
  const usedPct = disk.totalBytes > 0 ? Math.min(100, Math.max(0, (1 - disk.freeBytes / disk.totalBytes) * 100)) : 0;
  return {
    label: diskLabel(disk.uses),
    text: `${gigabytes(disk.freeBytes)} of ${gigabytes(disk.totalBytes)} free`,
    usedPct,
    low: disk.freeBytes < LOW_DISK_BYTES,
    path: disk.path,
  };
}
