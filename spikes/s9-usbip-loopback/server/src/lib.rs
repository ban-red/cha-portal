//! Spike S9: a userspace USB device served over USB/IP on a `socketpair`,
//! attached to the kernel's `vhci_hcd`. See the spike's README.

pub mod device;
pub mod engine;
pub mod usbip;

#[cfg(target_os = "linux")]
pub mod linux;
