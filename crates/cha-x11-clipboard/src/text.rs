//! Text and the targets we speak: which one of an owner's we ask for, and
//! what we answer a requestor's.

use x11rb::protocol::xproto::Atom;

use crate::atoms::Atoms;

/// A way to ask an owner for text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Source {
    pub target: Atom,
    /// Bytes are Latin-1 (`STRING`), not UTF-8.
    pub latin1: bool,
}

impl Source {
    pub fn utf8(atoms: &Atoms) -> Self {
        Self {
            target: atoms.utf8_string,
            latin1: false,
        }
    }

    pub fn latin1(atoms: &Atoms) -> Self {
        Self {
            target: atoms.string,
            latin1: true,
        }
    }
}

/// The best text type an owner's `TARGETS` offer: UTF-8, then Latin-1.
pub fn pick_source(offered: &[Atom], atoms: &Atoms) -> Option<Source> {
    let has = |atom| offered.contains(&atom);
    if has(atoms.utf8_string) {
        Some(Source::utf8(atoms))
    } else if has(atoms.mime_utf8) {
        Some(Source {
            target: atoms.mime_utf8,
            latin1: false,
        })
    } else if has(atoms.string) {
        Some(Source::latin1(atoms))
    } else {
        None
    }
}

/// What a requestor of our selection gets for `target`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answer {
    /// The list of targets we convert to.
    Targets,
    /// The time we took the selection.
    Timestamp,
    /// The text, as `type_`, in Latin-1 or UTF-8.
    Text {
        type_: Atom,
        latin1: bool,
    },
    Refuse,
}

pub fn answer_for(target: Atom, atoms: &Atoms) -> Answer {
    let utf8 = |type_| Answer::Text {
        type_,
        latin1: false,
    };
    if target == atoms.targets {
        Answer::Targets
    } else if target == atoms.timestamp {
        Answer::Timestamp
    } else if target == atoms.utf8_string || target == atoms.text {
        utf8(atoms.utf8_string)
    } else if target == atoms.mime_utf8 || target == atoms.mime_plain {
        utf8(target)
    } else if target == atoms.string {
        Answer::Text {
            type_: atoms.string,
            latin1: true,
        }
    } else {
        Answer::Refuse
    }
}

/// What `TARGETS` lists.
pub fn offered(atoms: &Atoms) -> [Atom; 7] {
    [
        atoms.targets,
        atoms.timestamp,
        atoms.utf8_string,
        atoms.string,
        atoms.text,
        atoms.mime_utf8,
        atoms.mime_plain,
    ]
}

/// Latin-1 is Unicode's first 256 code points.
pub fn latin1_to_utf8(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| char::from(b)).collect()
}

/// Characters Latin-1 lacks become `?`.
pub fn utf8_to_latin1(text: &str) -> Vec<u8> {
    text.chars()
        .map(|c| u8::try_from(c).unwrap_or(b'?'))
        .collect()
}

/// Whether server time `a` is before `b`, which wraps every 49 days. A time of
/// 0 (`CurrentTime`, or unknown) is before nothing.
pub fn before(a: u32, b: u32) -> bool {
    a != 0 && b != 0 && (a.wrapping_sub(b) as i32) < 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latin1_round_trip() {
        let bytes: Vec<u8> = (0..=255).collect();
        let text = latin1_to_utf8(&bytes);
        assert_eq!(text.chars().count(), 256);
        assert_eq!(utf8_to_latin1(&text), bytes);
        assert_eq!(latin1_to_utf8(b"caf\xe9"), "café");
    }

    #[test]
    fn latin1_replaces_what_it_lacks() {
        assert_eq!(utf8_to_latin1("é ✓ 🎉 z"), b"\xe9 ? ? z");
        assert_eq!(utf8_to_latin1(""), b"");
    }

    #[test]
    fn source_prefers_utf8() {
        let a = Atoms::numbered();
        let utf8 = Source::utf8(&a);
        let latin1 = Source::latin1(&a);
        assert_eq!(pick_source(&[a.string, a.utf8_string, 7], &a), Some(utf8));
        assert_eq!(pick_source(&[a.string, a.text], &a), Some(latin1));
        assert_eq!(
            pick_source(&[a.mime_utf8, a.string], &a),
            Some(Source {
                target: a.mime_utf8,
                latin1: false
            })
        );
        // An image, say.
        assert_eq!(pick_source(&[a.targets, 7, 8], &a), None);
        assert_eq!(pick_source(&[], &a), None);
    }

    #[test]
    fn answers() {
        let a = Atoms::numbered();
        assert_eq!(answer_for(a.targets, &a), Answer::Targets);
        assert_eq!(answer_for(a.timestamp, &a), Answer::Timestamp);
        let utf8 = |type_| Answer::Text {
            type_,
            latin1: false,
        };
        assert_eq!(answer_for(a.utf8_string, &a), utf8(a.utf8_string));
        assert_eq!(answer_for(a.text, &a), utf8(a.utf8_string));
        assert_eq!(answer_for(a.mime_utf8, &a), utf8(a.mime_utf8));
        assert_eq!(answer_for(a.mime_plain, &a), utf8(a.mime_plain));
        assert_eq!(
            answer_for(a.string, &a),
            Answer::Text {
                type_: a.string,
                latin1: true
            }
        );
        assert_eq!(answer_for(999, &a), Answer::Refuse);
        // Everything TARGETS lists is something we answer.
        for target in offered(&a) {
            assert_ne!(answer_for(target, &a), Answer::Refuse);
        }
    }

    #[test]
    fn server_time_wraps() {
        assert!(before(5, 10));
        assert!(!before(10, 5));
        assert!(!before(5, 5));
        // 0 is CurrentTime or unknown.
        assert!(!before(0, 10));
        assert!(!before(10, 0));
        // Across the 32-bit wrap: 0xffff_fff0 is just before 0x10.
        assert!(before(0xffff_fff0, 0x10));
        assert!(!before(0x10, 0xffff_fff0));
    }
}
