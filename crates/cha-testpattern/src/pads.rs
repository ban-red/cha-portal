//! The first gamepad in `/dev/input`, read with evdev, for the panel. Looks
//! again every second while there is none (or it went away).

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::draw::PadView;

const EV_KEY: u16 = 0x01;
const EV_ABS: u16 = 0x03;
/// BTN_A, BTN_B, BTN_X, BTN_Y, BTN_TL, BTN_TR, BTN_SELECT, BTN_START,
/// BTN_THUMBL, BTN_THUMBR, BTN_MODE: the panel's order.
const BUTTONS: [u16; 11] = [
    0x130, 0x131, 0x133, 0x134, 0x136, 0x137, 0x13a, 0x13b, 0x13d, 0x13e, 0x13c,
];

pub fn watch() -> Arc<Mutex<PadView>> {
    let view = Arc::new(Mutex::new(PadView::default()));
    let shared = Arc::clone(&view);
    std::thread::spawn(move || {
        loop {
            if let Some(path) = find_pad() {
                read(&path, &shared);
                *shared.lock().expect("pad lock") = PadView::default();
            }
            std::thread::sleep(Duration::from_secs(1));
        }
    });
    view
}

/// An event device that has gamepad buttons (BTN_A), by its sysfs capabilities.
fn find_pad() -> Option<PathBuf> {
    let mut nodes: Vec<String> = std::fs::read_dir("/dev/input")
        .ok()?
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.starts_with("event"))
        .collect();
    nodes.sort_by_key(|n| n[5..].parse::<u32>().unwrap_or(u32::MAX));
    nodes.into_iter().find_map(|name| {
        let caps =
            std::fs::read_to_string(format!("/sys/class/input/{name}/device/capabilities/key"))
                .ok()?;
        has_bit(&caps, 0x130).then(|| Path::new("/dev/input").join(name))
    })
}

/// Whether a sysfs bitmap (64-bit hex words, most significant first) has `bit`.
fn has_bit(bitmap: &str, bit: usize) -> bool {
    let words: Vec<&str> = bitmap.split_whitespace().rev().collect();
    words
        .get(bit / 64)
        .and_then(|w| u64::from_str_radix(w, 16).ok())
        .is_some_and(|w| (w >> (bit % 64)) & 1 == 1)
}

fn read(path: &Path, view: &Mutex<PadView>) {
    let Ok(mut file) = File::open(path) else {
        return;
    };
    view.lock().expect("pad lock").connected = true;
    // struct input_event: a 16-byte timeval, type, code, value.
    let mut event = [0u8; 24];
    while file.read_exact(&mut event).is_ok() {
        let kind = u16::from_ne_bytes([event[16], event[17]]);
        let code = u16::from_ne_bytes([event[18], event[19]]);
        let value = i32::from_ne_bytes([event[20], event[21], event[22], event[23]]);
        let mut v = view.lock().expect("pad lock");
        match (kind, code) {
            (EV_KEY, _) => {
                if let Some(i) = BUTTONS.iter().position(|b| *b == code) {
                    if value != 0 {
                        v.buttons |= 1 << i;
                    } else {
                        v.buttons &= !(1 << i);
                    }
                }
            }
            (EV_ABS, 0x00) => v.sticks[0].0 = value as i16,
            (EV_ABS, 0x01) => v.sticks[0].1 = value as i16,
            (EV_ABS, 0x03) => v.sticks[1].0 = value as i16,
            (EV_ABS, 0x04) => v.sticks[1].1 = value as i16,
            (EV_ABS, 0x02) => v.triggers[0] = value.clamp(0, 255) as u8,
            (EV_ABS, 0x05) => v.triggers[1] = value.clamp(0, 255) as u8,
            (EV_ABS, 0x10) => v.hat.0 = value.signum() as i8,
            (EV_ABS, 0x11) => v.hat.1 = value.signum() as i8,
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_key_bitmaps() {
        // An Xbox pad's key capabilities: bits 0x130.. in the fifth 64-bit word.
        let caps = "7cdb000000000000 0 0 0 0";
        assert!(has_bit(caps, 0x130));
        assert!(!has_bit(caps, 0x132));
        assert!(!has_bit("0", 0x130));
    }
}
