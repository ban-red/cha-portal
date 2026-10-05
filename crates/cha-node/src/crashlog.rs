//! What an environment's containers said before they died. The agent removes
//! them when one dies on its own, and their logs go with them, so it reads the
//! tail of each first ([`crate::docker::Docker::logs_tail`]) and keeps it:
//! on this node, in `<state>/logs/<environment>-<role>.log` for the newest
//! [`KEEP_ENVIRONMENTS`] environments ([`keep`]), and with the exit it
//! reports, where the owner can read it. From the tail comes a short reason
//! ([`reason`]) in place of "the streamer exited with code 1": a handful of
//! causes we have met, each with a sentence of its own, else the last line
//! that looks like an error.

use std::fs;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::process::Command;
use std::time::SystemTime;

/// Lines asked of the engine for each container.
pub const TAIL_LINES: usize = 200;
/// Lines reported to the portal for the whole environment.
pub const REPORT_LINES: usize = 60;
/// Characters of a reported line.
pub const REPORT_LINE_CHARS: usize = 300;
/// Characters of the detail that replaces "exited with code 1".
pub const DETAIL_CHARS: usize = 300;
/// Environments whose logs the node keeps.
pub const KEEP_ENVIRONMENTS: usize = 50;
/// Characters of the error line a reason falls back to.
const FALLBACK_CHARS: usize = 160;

/// The tail of one container's log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tail {
    /// `streamer` or `app`.
    pub role: String,
    /// Its lines as the engine gave them, each led by its timestamp.
    pub lines: Vec<String>,
}

/// Why an environment ended, from its containers' logs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reason {
    pub text: String,
    /// The GPU ran out of memory: worth saying who holds it ([`vram_now`]).
    pub gpu_memory: bool,
}

/// The causes we recognise, most specific first: each a sentence for the user,
/// and a test of a log line (lower case).
type Matcher = fn(&str) -> bool;

const KNOWN: [(&str, Matcher); 6] = [
    (
        "The node's GPU has no video encoder sessions left (NVENC)",
        |l| {
            l.contains("nv_enc_err_encoder_busy")
                || l.contains("nv_enc_err_no_encode_device")
                || l.contains("nv_enc_err_device_not_exist")
                || l.contains("too many nvenc sessions")
                || l.contains("maximum number of nvenc sessions")
        },
    ),
    ("The node's GPU is out of memory (VRAM)", |l| {
        l.contains("cuda_error_out_of_memory")
            || l.contains("nv_enc_err_out_of_memory")
            || l.contains("vk_error_out_of_device_memory")
            || (l.contains("out of memory")
                && [
                    "cudevice", "cuctx", "cumem", "cuda", "nvenc", "nvidia", "gpu", "vram",
                    "vulkan",
                ]
                .iter()
                .any(|w| l.contains(w)))
    }),
    (
        "The node can't make virtual gamepads: /dev/uinput isn't usable",
        |l| l.contains("uinput") && failed_to_open(l),
    ),
    (
        "The node can't make this controller: /dev/uhid isn't usable",
        |l| l.contains("uhid") && failed_to_open(l),
    ),
    (
        "A folder or volume the environment needs couldn't be mounted on the node",
        |l| {
            l.contains("bind source path does not exist")
                || l.contains("invalid mount config")
                || l.contains("error mounting")
                || l.contains("mounts denied")
                || l.contains("failed to mount")
        },
    ),
    (
        "A network port the environment needs is already in use on the node",
        |l| {
            l.contains("address already in use")
                || l.contains("port is already allocated")
                || l.contains("addrinuse")
        },
    ),
];

fn failed_to_open(line: &str) -> bool {
    [
        "no such file",
        "no such device",
        "permission denied",
        "failed to open",
        "can't open",
        "cannot open",
    ]
    .iter()
    .any(|w| line.contains(w))
}

/// Steam's sandbox (pressure-vessel, which uses bwrap) refused: AppArmor, or
/// the kernel not letting it make namespaces.
fn sandbox_blocked(line: &str) -> bool {
    (line.contains("apparmor") && (line.contains("denied") || line.contains("not loaded")))
        || ((line.contains("bwrap") || line.contains("pressure-vessel"))
            && [
                "permission denied",
                "operation not permitted",
                "creating new namespace failed",
                "setting up uid map",
            ]
            .iter()
            .any(|w| line.contains(w)))
}

/// A line of the log without what the engine and terminals put around it: the
/// timestamp `timestamps=1` leads with, colour codes, a trailing `\r`.
pub fn clean(line: &str) -> String {
    let line = match line.split_once(' ') {
        Some((stamp, rest)) if is_timestamp(stamp) => rest,
        _ => line,
    };
    let mut out = String::with_capacity(line.len());
    let mut chars = line.trim_end().chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            // `ESC [ … letter`
            if chars.peek() == Some(&'[') {
                chars.next();
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
        } else if !c.is_control() || c == '\t' {
            out.push(c);
        }
    }
    out.trim().to_string()
}

/// `2026-10-05T12:00:00.123456789Z`.
fn is_timestamp(word: &str) -> bool {
    let b = word.as_bytes();
    b.len() >= 20 && b[4] == b'-' && b[7] == b'-' && b[10] == b'T' && word.ends_with('Z')
}

/// Why the environment ended, if the logs say. `order` is the containers'
/// tails, the one that died first leading (a streamer that failed is the
/// cause of an app that did, not the other way round). A known cause comes
/// first; otherwise the last line that reads like an error, else the last
/// line that says anything. `died` is the role that exited and its code.
pub fn reason(order: &[&Tail], died: (&str, Option<i64>)) -> Option<Reason> {
    let cleaned: Vec<Vec<String>> = order
        .iter()
        .map(|t| t.lines.iter().map(|l| clean(l).to_lowercase()).collect())
        .collect();
    for lines in &cleaned {
        for (text, matches) in KNOWN {
            if lines.iter().any(|l| matches(l)) {
                return Some(Reason {
                    text: text.to_string(),
                    gpu_memory: text.contains("VRAM"),
                });
            }
        }
        if lines.iter().any(|l| sandbox_blocked(l)) {
            return Some(Reason {
                text: "Steam's sandbox was blocked on this node (AppArmor, or the kernel won't let it make namespaces)".into(),
                gpu_memory: false,
            });
        }
        // gamescope aborts without saying why: it is its exit and its name.
        let aborted = died.1 == Some(134)
            || lines.iter().any(|l| {
                l.contains("aborted") || l.contains("core dumped") || l.contains("sigabrt")
            });
        if aborted && lines.iter().any(|l| l.contains("gamescope")) {
            return Some(Reason {
                text: "gamescope crashed".into(),
                gpu_memory: false,
            });
        }
    }
    let text = order
        .iter()
        .find_map(|t| last_error_line(&t.lines))
        .or_else(|| order.iter().find_map(|t| last_line(&t.lines)))?;
    Some(Reason {
        text: shorten(&text, FALLBACK_CHARS),
        gpu_memory: false,
    })
}

/// The last line that says it is an error.
fn last_error_line(lines: &[String]) -> Option<String> {
    const WORDS: [&str; 5] = ["error", "panicked", "fatal", "abort", "failed"];
    lines.iter().rev().map(|l| clean(l)).find(|l| {
        let lower = l.to_lowercase();
        meaningful(l) && WORDS.iter().any(|w| lower.contains(w))
    })
}

fn last_line(lines: &[String]) -> Option<String> {
    lines.iter().rev().map(|l| clean(l)).find(|l| meaningful(l))
}

/// Not blank, and not rules or a bare frame of a stack.
fn meaningful(line: &str) -> bool {
    line.chars().filter(|c| c.is_alphanumeric()).count() >= 4
}

/// `text`, cut to `max` characters with an ellipsis.
pub fn shorten(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// What the user is shown: `<reason> (<what happened>)`, within
/// [`DETAIL_CHARS`] (the reason gives way, not the facts).
pub fn detail(what: &str, reason: Option<&str>) -> String {
    let Some(reason) = reason else {
        return shorten(what, DETAIL_CHARS);
    };
    let tail = format!(" ({what})");
    let room = DETAIL_CHARS.saturating_sub(tail.chars().count());
    if room < 20 {
        return shorten(&format!("{reason}{tail}"), DETAIL_CHARS);
    }
    format!("{}{tail}", shorten(reason, room))
}

/// The lines for the portal: each container's last lines under a heading,
/// [`REPORT_LINES`] in all, each cut to [`REPORT_LINE_CHARS`], without
/// timestamps (the file on the node keeps them).
pub fn report(tails: &[&Tail]) -> Vec<String> {
    let tails: Vec<&&Tail> = tails.iter().filter(|t| !t.lines.is_empty()).collect();
    if tails.is_empty() {
        return Vec::new();
    }
    let each = REPORT_LINES / tails.len() - 1;
    let mut out = Vec::new();
    for tail in tails {
        out.push(format!("--- {} ---", tail.role));
        let lines: Vec<String> = tail
            .lines
            .iter()
            .map(|l| clean(l))
            .filter(|l| !l.is_empty())
            .collect();
        let skip = lines.len().saturating_sub(each);
        out.extend(
            lines
                .into_iter()
                .skip(skip)
                .map(|l| l.chars().take(REPORT_LINE_CHARS).collect::<String>()),
        );
    }
    out
}

/// The GB figure for MiB, one decimal.
fn gb(mib: f64) -> String {
    format!("{:.1} GB", mib / 1024.0)
}

/// "VRAM 23.8 of 24.0 GB in use; python 17.3 GB, Cyberpunk2077.exe 4.7 GB",
/// from `nvidia-smi --query-gpu=memory.used,memory.total
/// --format=csv,noheader,nounits` and `--query-compute-apps=pid,process_name,
/// used_memory` (same format): the GPU with the most in use, and the three
/// processes holding most (a name's processes added up).
pub fn vram_context(memory: &str, apps: &str) -> Option<String> {
    let (used, total) = memory
        .lines()
        .filter_map(|line| {
            let (used, total) = line.split_once(',')?;
            Some((
                used.trim().parse::<f64>().ok()?,
                total.trim().parse::<f64>().ok()?,
            ))
        })
        .max_by(|a, b| a.0.total_cmp(&b.0))?;
    let mut held: Vec<(String, f64)> = Vec::new();
    for line in apps.lines() {
        let fields: Vec<&str> = line.split(',').map(str::trim).collect();
        if fields.len() < 3 {
            continue;
        }
        let Ok(mib) = fields[fields.len() - 1].parse::<f64>() else {
            continue;
        };
        let path = fields[1..fields.len() - 1].join(",");
        let name = path.rsplit(['/', '\\']).next().unwrap_or(&path).to_string();
        match held.iter_mut().find(|(n, _)| *n == name) {
            Some((_, total)) => *total += mib,
            None => held.push((name, mib)),
        }
    }
    held.sort_by(|a, b| b.1.total_cmp(&a.1));
    let mut text = format!(
        "VRAM {} of {} in use",
        gb(used).trim_end_matches(" GB"),
        gb(total)
    );
    if !held.is_empty() {
        let top: Vec<String> = held
            .iter()
            .take(3)
            .map(|(name, mib)| format!("{name} {}", gb(*mib)))
            .collect();
        text.push_str("; ");
        text.push_str(&top.join(", "));
    }
    Some(text)
}

/// [`vram_context`] for this node's GPU now, if `nvidia-smi` can say.
/// Blocking: a few milliseconds, unless the driver is wedged.
pub fn vram_now() -> Option<String> {
    let smi = |query: &str| {
        let output = Command::new("nvidia-smi")
            .args([query, "--format=csv,noheader,nounits"])
            .output()
            .ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
    };
    let memory = smi("--query-gpu=memory.used,memory.total")?;
    let apps = smi("--query-compute-apps=pid,process_name,used_memory").unwrap_or_default();
    vram_context(&memory, &apps)
}

/// Writes `tail` to `<dir>/<id>-<role>.log` (private to the agent's user),
/// then forgets all but the newest [`KEEP_ENVIRONMENTS`] environments' logs.
pub fn keep(dir: &Path, id: &str, tail: &Tail) -> std::io::Result<()> {
    // The id is the portal's; it names a file here.
    if id.is_empty()
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "an environment id that isn't a plain name",
        ));
    }
    fs::create_dir_all(dir)?;
    let path = dir.join(format!("{id}-{}.log", tail.role));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    for line in &tail.lines {
        writeln!(file, "{line}")?;
    }
    prune(dir, KEEP_ENVIRONMENTS)
}

/// Removes the logs of all but the `keep` environments most recently written
/// (an environment's logs are `<id>-streamer.log` and `<id>-app.log`).
pub fn prune(dir: &Path, keep: usize) -> std::io::Result<()> {
    let mut environments: std::collections::BTreeMap<
        String,
        (SystemTime, Vec<std::path::PathBuf>),
    > = Default::default();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(id) = ["-streamer.log", "-app.log"]
            .iter()
            .find_map(|suffix| name.strip_suffix(suffix))
        else {
            continue;
        };
        let modified = entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        let slot = environments
            .entry(id.to_string())
            .or_insert((SystemTime::UNIX_EPOCH, Vec::new()));
        slot.0 = slot.0.max(modified);
        slot.1.push(entry.path());
    }
    let mut newest: Vec<_> = environments.into_values().collect();
    newest.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
    for (_, files) in newest.into_iter().skip(keep) {
        for file in files {
            let _ = fs::remove_file(file);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tail(role: &str, lines: &[&str]) -> Tail {
        Tail {
            role: role.into(),
            lines: lines
                .iter()
                .map(|l| format!("2026-10-05T12:00:00.123456789Z {l}"))
                .collect(),
        }
    }

    fn why(role: &str, code: i64, lines: &[&str]) -> Option<String> {
        let t = tail(role, lines);
        reason(&[&t], (role, Some(code))).map(|r| r.text)
    }

    #[test]
    fn strips_timestamps_and_colour() {
        assert_eq!(
            clean("2026-10-05T12:00:00.123456789Z \u{1b}[31mError:\u{1b}[0m boom\r"),
            "Error: boom"
        );
        assert_eq!(clean("plain words"), "plain words");
    }

    #[test]
    fn the_exact_cuda_line_from_a_full_gpu() {
        let t = tail(
            "streamer",
            &[
                "starting",
                "Error: cuDevicePrimaryCtxRetain failed: out of memory",
            ],
        );
        let r = reason(&[&t], ("streamer", Some(1))).unwrap();
        assert_eq!(r.text, "The node's GPU is out of memory (VRAM)");
        assert!(r.gpu_memory);
    }

    #[test]
    fn the_known_causes() {
        let cases: [(&str, &[&str], &str); 11] = [
            (
                "streamer",
                &["CUDA_ERROR_OUT_OF_MEMORY"],
                "out of memory (VRAM)",
            ),
            (
                "streamer",
                &["NvEncOpenEncodeSessionEx: NV_ENC_ERR_OUT_OF_MEMORY"],
                "out of memory (VRAM)",
            ),
            (
                "app",
                &["vk_error_out_of_device_memory"],
                "out of memory (VRAM)",
            ),
            (
                "streamer",
                &["NV_ENC_ERR_ENCODER_BUSY"],
                "no video encoder sessions",
            ),
            (
                "streamer",
                &["Error: /dev/uinput: Permission denied"],
                "/dev/uinput",
            ),
            (
                "streamer",
                &["can't open /dev/uhid: No such file or directory"],
                "/dev/uhid",
            ),
            (
                "app",
                &["bind source path does not exist: /mnt/x"],
                "couldn't be mounted",
            ),
            ("streamer", &["listen: Address already in use"], "port"),
            (
                "app",
                &["bwrap: Creating new namespace failed: Operation not permitted"],
                "Steam's sandbox",
            ),
            (
                "app",
                &["audit: apparmor=\"DENIED\" operation=\"userns_create\""],
                "Steam's sandbox",
            ),
            (
                "app",
                &["gamescope: wlserver: starting", "Aborted (core dumped)"],
                "gamescope crashed",
            ),
        ];
        for (role, lines, expect) in cases {
            let got = why(role, 134, lines).unwrap();
            assert!(got.contains(expect), "{lines:?}: {got}");
        }
        // The bare 134 and gamescope's name is enough; a bare 134 isn't.
        assert_eq!(
            why("app", 134, &["gamescope: init"]).unwrap(),
            "gamescope crashed"
        );
        assert_eq!(
            why("app", 134, &["something else"]).unwrap(),
            "something else"
        );
        // Ordinary RAM running out isn't the GPU.
        assert!(
            !why("app", 137, &["Out of memory: Killed process 4"])
                .unwrap()
                .contains("VRAM")
        );
    }

    #[test]
    fn the_container_that_died_first_is_asked_first() {
        let streamer = tail("streamer", &["CUDA_ERROR_OUT_OF_MEMORY"]);
        let app = tail("app", &["error: window gone"]);
        let r = reason(&[&app, &streamer], ("app", Some(1))).unwrap();
        assert!(r.gpu_memory);
        let r = reason(&[&app], ("app", Some(1))).unwrap();
        assert_eq!(r.text, "error: window gone");
    }

    #[test]
    fn otherwise_the_last_error_line_or_the_last_line() {
        let lines = [
            "Error: first thing",
            "panicked at src/main.rs:3: the real one",
            "   ",
            "----",
            "cleaning up",
        ];
        assert_eq!(
            why("streamer", 101, &lines).unwrap(),
            "panicked at src/main.rs:3: the real one"
        );
        assert_eq!(
            why("app", 1, &["starting up", "", "***", "shutting down"]).unwrap(),
            "shutting down"
        );
        assert_eq!(why("app", 1, &["", " "]), None);
        let long = format!("Error: {}", "x".repeat(400));
        let got = why("app", 1, &[&long]).unwrap();
        assert_eq!(got.chars().count(), FALLBACK_CHARS);
        assert!(got.ends_with('…'));
    }

    #[test]
    fn the_detail_keeps_its_facts_within_the_cap() {
        assert_eq!(
            detail("the streamer exited with code 1", Some("The GPU is full")),
            "The GPU is full (the streamer exited with code 1)"
        );
        assert_eq!(detail("the app exited", None), "the app exited");
        let long = "r".repeat(500);
        let got = detail("the app exited with code 134", Some(&long));
        assert_eq!(got.chars().count(), DETAIL_CHARS);
        assert!(got.ends_with("… (the app exited with code 134)"));
    }

    #[test]
    fn the_report_is_the_last_lines_of_each_without_timestamps() {
        let many: Vec<String> = (0..100).map(|n| format!("line {n}")).collect();
        let refs: Vec<&str> = many.iter().map(String::as_str).collect();
        let streamer = tail("streamer", &refs);
        let app = tail("app", &["x".repeat(500).as_str()]);
        let out = report(&[&streamer, &app]);
        assert_eq!(out[0], "--- streamer ---");
        assert_eq!(out[1], "line 71");
        assert_eq!(out[29], "line 99");
        assert_eq!(out[30], "--- app ---");
        assert_eq!(out[31].chars().count(), REPORT_LINE_CHARS);
        assert!(out.len() <= REPORT_LINES);
        assert!(report(&[&tail("app", &[])]).is_empty());
    }

    #[test]
    fn vram_from_a_full_gpu() {
        let memory = "24370, 24564\n";
        let apps = "\
1201, /usr/bin/python3, 17731
2310, Z:\\games\\Cyberpunk2077.exe, 4812
1202, /usr/bin/python3, 100
3000, [Not Found], 300
3001, steam, 50
4000, odd, [N/A]
";
        assert_eq!(
            vram_context(memory, apps).unwrap(),
            "VRAM 23.8 of 24.0 GB in use; python3 17.4 GB, Cyberpunk2077.exe 4.7 GB, [Not Found] 0.3 GB"
        );
        assert_eq!(
            vram_context("100, 8192", "").unwrap(),
            "VRAM 0.1 of 8.0 GB in use"
        );
        assert_eq!(vram_context("garbage", ""), None);
    }

    #[test]
    fn keeps_the_newest_environments_logs() {
        let dir = tempfile::tempdir().unwrap();
        for n in 0..4 {
            let id = format!("env{n}");
            for role in ["streamer", "app"] {
                let path = dir.path().join(format!("{id}-{role}.log"));
                fs::write(&path, "x").unwrap();
                let when = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1000 + n);
                fs::File::options()
                    .write(true)
                    .open(&path)
                    .unwrap()
                    .set_modified(when)
                    .unwrap();
            }
        }
        fs::write(dir.path().join("notes.txt"), "mine").unwrap();
        prune(dir.path(), 2).unwrap();
        let mut left: Vec<String> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(
            left,
            [
                "env2-app.log",
                "env2-streamer.log",
                "env3-app.log",
                "env3-streamer.log",
                "notes.txt"
            ]
        );
    }

    #[test]
    fn a_kept_log_is_private_and_written_whole() {
        let dir = tempfile::tempdir().unwrap();
        let logs = dir.path().join("logs");
        let t = tail("app", &["one", "two"]);
        keep(&logs, "e1", &t).unwrap();
        keep(&logs, "e1", &t).unwrap();
        let path = logs.join("e1-app.log");
        let text = fs::read_to_string(&path).unwrap();
        assert_eq!(text.lines().count(), 2);
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
