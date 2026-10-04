//! The app's setup status (P2.1): what a long first-run setup is doing while
//! the picture is still black (Steam's 500 MB download).
//!
//! The app writes one JSON object to `status` in the shared runtime dir and
//! replaces it atomically (a temp file in the same dir, then a rename):
//! `{"label": "Downloading Steam", "done": 123, "total": 496, "unit": "MB"}`.
//! - Only `label` is required. With no `total` the progress is indeterminate;
//!   `done` is held to `total` and `unit` names both.
//! - Removing the file, or writing `{}` (or no label), clears it.
//! - A file that doesn't parse, or is over 4 KiB, changes nothing: the app may
//!   not have replaced it atomically.
//!
//! A plain thread reads the file every 250 ms (an open and a few dozen bytes,
//! with no inotify binding to add) and publishes the latest on a watch
//! channel, which every session forwards to its page (`control::next_status`).

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;
use tokio::sync::watch;
use tracing::{debug, warn};

/// How often the file is read.
const POLL: Duration = Duration::from_millis(250);
/// A status is a few dozen bytes; a bigger file isn't one.
const MAX_BYTES: usize = 4096;
/// Longer labels and units are cut (in characters).
const MAX_LABEL: usize = 120;
const MAX_UNIT: usize = 16;

/// What the app is doing, as the page gets it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Status {
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub done: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
}

/// The environment's latest status (None: nothing to show).
pub type StatusWatch = watch::Receiver<Option<Status>>;

/// What one read of the file says.
#[derive(Debug, PartialEq)]
enum Reading {
    Set(Status),
    Cleared,
    /// Not a status (yet): the last one stands.
    Unusable,
}

fn parse(bytes: &[u8]) -> Reading {
    if bytes.len() > MAX_BYTES {
        return Reading::Unusable;
    }
    let Ok(Value::Object(fields)) = serde_json::from_slice(bytes) else {
        return Reading::Unusable;
    };
    let text = |key: &str, max: usize| {
        let text = fields.get(key)?.as_str()?.trim();
        (!text.is_empty()).then(|| text.chars().take(max).collect::<String>())
    };
    let Some(label) = text("label", MAX_LABEL) else {
        return Reading::Cleared;
    };
    let number = |key: &str| {
        fields
            .get(key)
            .and_then(Value::as_f64)
            .filter(|v| v.is_finite() && *v >= 0.0)
    };
    let total = number("total").filter(|t| *t > 0.0);
    let done = number("done").map(|d| total.map_or(d, |t| d.min(t)));
    Reading::Set(Status {
        label,
        done,
        total,
        unit: text("unit", MAX_UNIT),
    })
}

fn read(path: &Path) -> Reading {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Reading::Cleared,
        Err(_) => return Reading::Unusable,
    };
    // One byte past the limit tells a file that's too big from one that fills it.
    let mut bytes = Vec::new();
    match file
        .by_ref()
        .take(MAX_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
    {
        Ok(_) => parse(&bytes),
        Err(_) => Reading::Unusable,
    }
}

/// The watch on the app's `status` file.
pub struct SetupStatus {
    watch: StatusWatch,
}

impl Default for SetupStatus {
    /// A status that never changes.
    fn default() -> Self {
        Self {
            watch: watch::channel(None).1,
        }
    }
}

impl SetupStatus {
    /// Follows the file at `path` until the last watch is dropped.
    pub fn start(path: PathBuf) -> Self {
        let (publish, watch) = watch::channel(None);
        let spawned = std::thread::Builder::new()
            .name("setup-status".into())
            .spawn(move || {
                let mut unusable = false;
                while !publish.is_closed() {
                    let next = match read(&path) {
                        Reading::Set(status) => Some(Some(status)),
                        Reading::Cleared => Some(None),
                        Reading::Unusable => None,
                    };
                    match next {
                        Some(next) => {
                            unusable = false;
                            publish.send_if_modified(|current| {
                                let changed = *current != next;
                                if changed {
                                    debug!(status = ?next, "setup status");
                                    *current = next;
                                }
                                changed
                            });
                        }
                        // Said once per run of them.
                        None if !unusable => {
                            unusable = true;
                            warn!(path = %path.display(), "the setup status isn't a status; keeping the last");
                        }
                        None => {}
                    }
                    std::thread::sleep(POLL);
                }
            });
        if let Err(err) = spawned {
            warn!("no setup status: {err}");
        }
        Self { watch }
    }

    /// The status for a new session: the current one counts as new (a late
    /// joiner gets it at once), nothing does when there's none.
    pub fn subscribe(&self) -> StatusWatch {
        let mut watch = self.watch.clone();
        if watch.borrow().is_some() {
            watch.mark_changed();
        } else {
            watch.mark_unchanged();
        }
        watch
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(label: &str, done: Option<f64>, total: Option<f64>, unit: Option<&str>) -> Reading {
        Reading::Set(Status {
            label: label.into(),
            done,
            total,
            unit: unit.map(Into::into),
        })
    }

    #[test]
    fn a_full_status_parses() {
        let json = br#"{"label": "Downloading Steam", "done": 123, "total": 496, "unit": "MB"}"#;
        assert_eq!(
            parse(json),
            set("Downloading Steam", Some(123.0), Some(496.0), Some("MB"))
        );
    }

    #[test]
    fn only_the_label_is_required() {
        assert_eq!(
            parse(br#"{"label":"Unpacking Steam"}"#),
            set("Unpacking Steam", None, None, None)
        );
        // Unknown fields are for later versions.
        assert_eq!(
            parse(br#"{"label":" x ","eta":4,"unit":""}"#),
            set("x", None, None, None)
        );
    }

    #[test]
    fn nothing_to_show_clears() {
        assert_eq!(parse(b"{}"), Reading::Cleared);
        assert_eq!(parse(br#"{"label":"  "}"#), Reading::Cleared);
        assert_eq!(parse(br#"{"label":7,"done":1}"#), Reading::Cleared);
    }

    #[test]
    fn what_isnt_a_status_changes_nothing() {
        for bytes in [&b""[..], b"{", b"[]", b"\"x\"", b"null", b"{\"label\":"] {
            assert_eq!(parse(bytes), Reading::Unusable, "{bytes:?}");
        }
        let big = format!(r#"{{"label":"{}"}}"#, "x".repeat(MAX_BYTES));
        assert_eq!(parse(big.as_bytes()), Reading::Unusable);
    }

    #[test]
    fn numbers_are_made_sane() {
        // done is held to total; a total that isn't positive is none.
        assert_eq!(
            parse(br#"{"label":"a","done":600,"total":500}"#),
            set("a", Some(500.0), Some(500.0), None)
        );
        assert_eq!(
            parse(br#"{"label":"a","done":3,"total":0}"#),
            set("a", Some(3.0), None, None)
        );
        assert_eq!(
            parse(br#"{"label":"a","done":-1,"total":"9"}"#),
            set("a", None, None, None)
        );
        assert_eq!(
            parse(br#"{"label":"a","done":2.5,"total":10,"unit":"GB"}"#),
            set("a", Some(2.5), Some(10.0), Some("GB"))
        );
    }

    #[test]
    fn long_text_is_cut_by_characters() {
        let label = "é".repeat(MAX_LABEL + 10);
        let json = format!(r#"{{"label":"{label}","unit":"{}"}}"#, "ü".repeat(40));
        let Reading::Set(status) = parse(json.as_bytes()) else {
            panic!("a status");
        };
        assert_eq!(status.label.chars().count(), MAX_LABEL);
        assert_eq!(status.unit.unwrap().chars().count(), MAX_UNIT);
    }

    struct Dir(PathBuf);

    impl Dir {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("cha-status-{name}-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        /// The app's way: a temp file beside it, then a rename.
        fn write(&self, json: &str) {
            let tmp = self.0.join("status.tmp");
            std::fs::write(&tmp, json).unwrap();
            std::fs::rename(tmp, self.0.join("status")).unwrap();
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    async fn next(watch: &mut StatusWatch) -> Option<Status> {
        tokio::time::timeout(Duration::from_secs(5), watch.changed())
            .await
            .expect("a change within 5 s")
            .expect("the watcher runs");
        watch.borrow_and_update().clone()
    }

    #[tokio::test]
    async fn changes_reach_the_watch_once_each() {
        let dir = Dir::new("changes");
        let status = SetupStatus::start(dir.0.join("status"));
        let mut watch = status.subscribe();

        dir.write(r#"{"label":"Downloading","done":1,"total":4,"unit":"MB"}"#);
        let first = next(&mut watch).await.expect("a status");
        assert_eq!(
            (first.label.as_str(), first.done),
            ("Downloading", Some(1.0))
        );

        // The same status again is no news, a new one is.
        dir.write(r#"{"unit":"MB","total":4,"done":1,"label":"Downloading"}"#);
        dir.write(r#"{"label":"Downloading","done":2,"total":4,"unit":"MB"}"#);
        assert_eq!(next(&mut watch).await.unwrap().done, Some(2.0));

        // A half-written file leaves the last status standing.
        std::fs::write(dir.0.join("status"), r#"{"label":"Dow"#).unwrap();
        tokio::time::sleep(POLL * 3).await;
        assert!(!watch.has_changed().unwrap());
        assert_eq!(watch.borrow().as_ref().unwrap().done, Some(2.0));

        // Cleared by `{}`, and again by removing the file.
        dir.write("{}");
        assert_eq!(next(&mut watch).await, None);
        dir.write(r#"{"label":"Installing"}"#);
        assert_eq!(next(&mut watch).await.unwrap().label, "Installing");
        std::fs::remove_file(dir.0.join("status")).unwrap();
        assert_eq!(next(&mut watch).await, None);
    }

    #[tokio::test]
    async fn a_late_joiner_gets_the_current_status_and_only_that() {
        let dir = Dir::new("late");
        let status = SetupStatus::start(dir.0.join("status"));
        let mut early = status.subscribe();
        // Nothing yet: nothing is news.
        assert!(!early.has_changed().unwrap());

        dir.write(r#"{"label":"Extracting","done":5,"total":9}"#);
        next(&mut early).await.expect("a status");

        let mut late = status.subscribe();
        assert!(
            late.has_changed().unwrap(),
            "the current status is new to it"
        );
        assert_eq!(
            late.borrow_and_update().as_ref().unwrap().label,
            "Extracting"
        );
        assert!(!late.has_changed().unwrap());

        dir.write("{}");
        assert_eq!(next(&mut late).await, None);
        // Cleared: a joiner now has nothing to be told.
        assert!(!status.subscribe().has_changed().unwrap());
    }
}
