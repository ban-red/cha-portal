"""Tests for steam-status: `python3 -m unittest discover -s images/steam`.

The log lines are Steam's own (a first launch's bootstrap log, trimmed). The
watcher runs for real in a few tests, against a pretend home and runtime dir.
"""

import importlib.machinery
import importlib.util
import json
import os
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
SCRIPT = os.path.join(HERE, "steam-status")
loader = importlib.machinery.SourceFileLoader("steam_status", SCRIPT)
spec = importlib.util.spec_from_loader("steam_status", loader)
steam_status = importlib.util.module_from_spec(spec)
loader.exec_module(steam_status)

BOOTSTRAP = steam_status.BOOTSTRAP_LOG
UI = steam_status.UI_LOG

# A first launch: the updater downloads, unpacks and installs, then Steam
# starts again as the client.
FIRST_LAUNCH = """\

[2026-10-04 21:59:07] Startup - updater built Jun 24 2026 23:24:37
[2026-10-04 21:59:07] Startup - Steam Client launched with: '/home/cha/.local/share/Steam/ubuntu12_32/steam' '-srt-logger-opened' '-gamepadui'
[2026-10-04 21:59:07] Failed to load cached hosts file (File 'update_hosts_cached.vdf' not found), using defaults
[2026-10-04 21:59:07] Verifying installation...
[2026-10-04 21:59:07] Unable to read and verify install manifest /home/cha/.local/share/Steam/package/steam_client_ubuntu12.installed
[2026-10-04 21:59:07] Verification complete
[2026-10-04 21:59:07] Downloading Update...
[2026-10-04 21:59:07] Checking for available update...
[2026-10-04 21:59:07] Downloading manifest: https://client-update.steamstatic.com/steam_client_ubuntu12
[2026-10-04 21:59:07] Downloaded new manifest: /steam_client_ubuntu12 version 1788652215, installed version 0, existing pending version 0
[2026-10-04 21:59:08] Package file tenfoot_images_all.zip.vz.193cb8c4eb4446698ea2c0a9e8c4e6b6a623dac7_5572671 missing or incorrect size
[2026-10-04 21:59:08] Add pending download: https://client-update.steamstatic.com/tenfoot_images_all.zip.vz.193cb8c4eb4446698ea2c0a9e8c4e6b6a623dac7_5572671
[2026-10-04 21:59:08] Manifest download: send request
[2026-10-04 21:59:09] Downloading update (51,023 of 496,367 KB)...
[2026-10-04 21:59:09] Downloading update (64,130 of 496,367 KB)...
[2026-10-04 21:59:09] Downloading update (64,500 of 496,367 KB)...
[2026-10-04 21:59:28] Downloading update (493,420 of 496,367 KB)...
[2026-10-04 21:59:33] Downloading update (496,367 of 496,367 KB)...
[2026-10-04 21:59:33] Download Complete.
[2026-10-04 21:59:33] Saving metrics to disk (/home/cha/.local/share/Steam/package/steam_client_metrics.bin)
[2026-10-04 21:59:33] uninstalled manifest found in /home/cha/.local/share/Steam/package/steam_client_ubuntu12 (1).
[2026-10-04 21:59:33] Extracting package...
[2026-10-04 21:59:42] Installing update...
[2026-10-04 21:59:48] Cleaning up...
[2026-10-04 21:59:48] Update complete, launching...
[2026-10-04 21:59:48] Shutdown


[2026-10-04 21:59:51] Startup - updater built Sep  3 2026 01:31:48
[2026-10-04 21:59:51] Startup - Steam Client launched with: '/home/cha/.local/share/Steam/ubuntu12_32/steam' '-srt-logger-opened' '-gamepadui'
[2026-10-04 21:59:51] Loading cached metrics from disk (/home/cha/.local/share/Steam/package/steam_client_metrics.bin)
[2026-10-04 21:59:51] Verifying installation...
[2026-10-04 21:59:51] Verifying file sizes only
[2026-10-04 21:59:51] Verification complete
"""

# A later launch of a home that is set up: nothing to download.
LATER_LAUNCH = """\


[2026-10-04 22:22:25] Startup - updater built Sep  3 2026 01:31:48
[2026-10-04 22:22:25] Startup - Steam Client launched with: '/home/cha/.local/share/Steam/ubuntu12_32/steam' '-srt-logger-opened' '-gamepadui'
[2026-10-04 22:22:26] Loading cached metrics from disk (/home/cha/.local/share/Steam/package/steam_client_metrics.bin)
[2026-10-04 22:22:26] Checking for update on startup
[2026-10-04 22:22:26] Checking for available updates...
[2026-10-04 22:22:26] Downloading manifest: https://client-update.steamstatic.com/steam_client_ubuntu12
[2026-10-04 22:22:26] Manifest download: waiting for download to finish
[2026-10-04 22:22:26] Download skipped: /steam_client_ubuntu12 version 1788652215, installed version 1788652215, existing pending version 0
[2026-10-04 22:22:26] Nothing to do
[2026-10-04 22:22:26] Verifying installation...
[2026-10-04 22:22:26] Verifying all executable checksums
[2026-10-04 22:22:28] Verification complete
"""

# What steamui_html.txt logs as the UI comes up.
UI_STARTING = """\

[2026-10-04 22:28:50] Client version: 1788652215
[2026-10-04 22:28:50] Started webhelper process 1164
[2026-10-04 22:28:50] CreateBrowser id:860062213 type:12 flags:1000000 (-2147483648, -2147483648) 0x0
[2026-10-04 22:28:52] CreateResponse: id:860062213 handle:65536
[2026-10-04 22:28:52] BrowserReady: handle:65536
[2026-10-04 22:28:53] GetDesiredSteamUIWindows: starting processing of 1 windows
[2026-10-04 22:28:56] PopupHTMLWindow: idx:131073 handle:65536
[2026-10-04 22:28:56] BrowserReady: handle:131073
"""

STARTING = {"label": "Starting Steam"}


def feed(setup, log, text):
    """The distinct statuses `text` takes the setup through, in order."""
    seen = []
    for line in text.splitlines():
        setup.feed(log, line)
        if not seen or seen[-1] != setup.status:
            seen.append(setup.status)
    return seen


def wait_for(check, what, timeout=10.0):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        value = check()
        if value:
            return value
        time.sleep(0.05)
    raise AssertionError(f"timed out waiting for {what}")


class SetupTests(unittest.TestCase):
    def test_a_first_launch_goes_from_download_to_install_to_start(self):
        setup = steam_status.Setup()
        seen = feed(setup, BOOTSTRAP, FIRST_LAUNCH)
        download = {"label": "Downloading Steam", "unit": "MB", "total": 496}
        self.assertEqual(
            seen,
            [
                STARTING,
                {"label": "Downloading Steam"},
                {**download, "done": 51},
                {**download, "done": 64},
                {**download, "done": 493},
                {**download, "done": 496},
                {"label": "Unpacking Steam"},
                {"label": "Installing Steam"},
                STARTING,
            ],
        )
        self.assertFalse(setup.up)

    def test_a_launch_with_nothing_to_download_only_starts(self):
        setup = steam_status.Setup()
        self.assertEqual(feed(setup, BOOTSTRAP, LATER_LAUNCH), [STARTING])
        self.assertFalse(setup.up)

    def test_the_ui_ends_it(self):
        setup = steam_status.Setup()
        seen = feed(setup, UI, UI_STARTING)
        self.assertEqual(seen, [STARTING, None])
        self.assertTrue(setup.up)

    def test_only_the_ui_log_says_the_ui_is_up(self):
        setup = steam_status.Setup()
        setup.feed(BOOTSTRAP, "[2026-10-04 22:28:56] PopupHTMLWindow: idx:1 handle:2")
        self.assertFalse(setup.up)
        setup.feed("cef_log.txt", "PopupHTMLWindow")
        self.assertFalse(setup.up)

    def test_a_progress_line_in_other_words_is_still_a_download(self):
        setup = steam_status.Setup()
        setup.feed(BOOTSTRAP, "[2026-10-04 21:59:09] Downloading update (51 of 496 MB)...")
        self.assertEqual(setup.status, {"label": "Downloading Steam"})

    def test_megabytes_are_steams_thousands(self):
        self.assertEqual(steam_status.megabytes("496,367"), 496)
        self.assertEqual(steam_status.megabytes("51,023"), 51)
        self.assertEqual(steam_status.megabytes("999"), 0)
        self.assertEqual(steam_status.megabytes("1,234,567"), 1234)


class Scratch(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.mkdtemp(prefix="steam-status-test-")
        self.addCleanup(shutil.rmtree, self.dir, ignore_errors=True)

    def path(self, *parts):
        return os.path.join(self.dir, *parts)

    def append(self, path, text):
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "a") as f:
            f.write(text)


class TailTests(Scratch):
    def test_a_log_that_isnt_there_yet_is_all_new_once_it_is(self):
        tail = steam_status.Tail(self.path("logs", "bootstrap_log.txt"))
        self.assertEqual(tail.lines(), [])
        self.append(self.path("logs", "bootstrap_log.txt"), "one\ntwo\n")
        self.assertEqual(tail.lines(), ["one", "two"])
        self.assertEqual(tail.lines(), [])

    def test_earlier_launches_are_not_news(self):
        log = self.path("bootstrap_log.txt")
        self.append(log, "old 1\nold 2\n")
        tail = steam_status.Tail(log)
        self.assertEqual(tail.lines(), [])
        self.append(log, "new\n")
        self.assertEqual(tail.lines(), ["new"])

    def test_a_line_is_held_until_it_ends(self):
        log = self.path("bootstrap_log.txt")
        tail = steam_status.Tail(log)
        self.append(log, "Downloading update (1,0")
        self.assertEqual(tail.lines(), [])
        self.append(log, "00 of 2,000 KB)...\nnext")
        self.assertEqual(tail.lines(), ["Downloading update (1,000 of 2,000 KB)..."])
        self.append(log, "\n")
        self.assertEqual(tail.lines(), ["next"])

    def test_a_replaced_log_starts_over(self):
        log = self.path("bootstrap_log.txt")
        self.append(log, "a long first line\n")
        tail = steam_status.Tail(log)
        os.remove(log)
        self.append(log, "short\n")
        self.assertEqual(tail.lines(), ["short"])

    def test_bytes_that_arent_utf8_dont_stop_it(self):
        log = self.path("bootstrap_log.txt")
        tail = steam_status.Tail(log)
        with open(log, "wb") as f:
            f.write(b"caf\xe9\nok\n")
        self.assertEqual(tail.lines(), ["caf�", "ok"])


class PublishTests(Scratch):
    def test_it_replaces_the_file_whole_and_leaves_no_temp_file(self):
        status = self.path("status")
        steam_status.publish(status, {"label": "Installing Steam"})
        with open(status) as f:
            self.assertEqual(json.load(f), {"label": "Installing Steam"})
        steam_status.publish(status, {"label": "Starting Steam"})
        with open(status) as f:
            self.assertEqual(json.load(f), {"label": "Starting Steam"})
        self.assertEqual(os.listdir(self.dir), ["status"])
        steam_status.publish(status, None)
        self.assertEqual(os.listdir(self.dir), [])
        steam_status.publish(status, None)

    def test_a_directory_that_isnt_there_is_not_an_error(self):
        steam_status.publish(self.path("missing", "status"), {"label": "x"})
        steam_status.publish(self.path("missing", "status"), None)


class WatcherTests(Scratch):
    """The script as the image runs it: its own process, a pretend home."""

    def setUp(self):
        super().setUp()
        self.home = self.path("home")
        self.runtime = self.path("run")
        os.makedirs(self.runtime)
        self.logs = os.path.join(self.home, ".local/share/Steam/logs")
        self.status = os.path.join(self.runtime, "status")
        self.env = {**os.environ, "HOME": self.home, "XDG_RUNTIME_DIR": self.runtime}

    def start(self):
        process = subprocess.Popen([sys.executable, SCRIPT], env=self.env)
        self.addCleanup(process.wait)
        self.addCleanup(process.kill)
        return process

    def current(self):
        try:
            with open(self.status) as f:
                return json.load(f)
        except (OSError, ValueError):
            return None

    def expect(self, status, timeout=10.0):
        wait_for(lambda: self.current() == status, f"status {status}, have {self.current()}", timeout)

    def test_a_first_launch_from_start_to_the_ui(self):
        # The home has earlier launches' logs: none of that is news.
        self.append(os.path.join(self.logs, BOOTSTRAP), LATER_LAUNCH + FIRST_LAUNCH)
        self.append(os.path.join(self.logs, UI), UI_STARTING)
        watcher = self.start()
        self.expect(STARTING)
        time.sleep(1)
        self.assertEqual(self.current(), STARTING)
        self.assertIsNone(watcher.poll())

        lines = FIRST_LAUNCH.splitlines(keepends=True)
        log = os.path.join(self.logs, BOOTSTRAP)
        start = next(i for i, line in enumerate(lines) if "Downloading update (51" in line)
        self.append(log, "".join(lines[start : start + 2]))
        self.expect({"label": "Downloading Steam", "done": 64, "total": 496, "unit": "MB"})
        end = next(i for i, line in enumerate(lines) if "Extracting" in line)
        self.append(log, "".join(lines[start + 2 : end + 1]))
        self.expect({"label": "Unpacking Steam"})
        self.append(log, "[2026-10-04 21:59:42] Installing update...\n")
        self.expect({"label": "Installing Steam"})
        self.append(log, "[2026-10-04 21:59:48] Update complete, launching...\n")
        self.expect(STARTING)

        # The client's UI log: its lines before the window don't end it.
        ui = os.path.join(self.logs, UI)
        self.append(ui, "[2026-10-04 22:30:01] Started webhelper process 99\n[2026-10-04 22:30:02] BrowserReady: handle:65536\n")
        time.sleep(1)
        self.assertEqual(self.current(), STARTING)
        self.append(ui, "[2026-10-04 22:30:04] PopupHTMLWindow: idx:131073 handle:65536\n")
        wait_for(lambda: not os.path.exists(self.status), "the status to go")
        self.assertEqual(watcher.wait(timeout=10), 0)

    def test_a_home_with_no_logs_yet(self):
        watcher = self.start()
        self.expect(STARTING)
        self.append(os.path.join(self.logs, BOOTSTRAP), "[2026-10-04 21:59:09] Downloading update (51,023 of 496,367 KB)...\n")
        self.expect({"label": "Downloading Steam", "done": 51, "total": 496, "unit": "MB"})
        watcher.terminate()
        self.assertEqual(watcher.wait(timeout=10), 0)
        self.assertFalse(os.path.exists(self.status), "SIGTERM clears the status")

    def test_it_ends_with_its_parent(self):
        shell = subprocess.Popen(["sh", "-c", f'"{sys.executable}" "{SCRIPT}" & sleep 1.5'], env=self.env)
        self.addCleanup(shell.wait)
        self.expect(STARTING)
        self.assertEqual(shell.wait(timeout=10), 0)
        wait_for(lambda: not os.path.exists(self.status), "the status to go")

    @unittest.skipUnless(os.path.isdir("/proc/self"), "needs /proc")
    def test_a_web_helper_that_has_run_a_while_ends_it(self):
        # A process named as Steam's web helper, and no log line to say so (a
        # script's process is named for the script).
        helper = self.path("steamwebhelper")
        with open(helper, "w") as f:
            f.write(f"#!{sys.executable}\nimport time\ntime.sleep(60)\n")
        os.chmod(helper, 0o755)
        sleeper = subprocess.Popen([helper])
        self.addCleanup(sleeper.wait)
        self.addCleanup(sleeper.kill)
        self.assertTrue(steam_status.running("steamwebhelper"))
        steam_status.HELPER_GRACE = 1.0
        os.environ.update(HOME=self.home, XDG_RUNTIME_DIR=self.runtime)
        # main() takes SIGTERM over, and a hang is the failure to catch.
        term = signal.getsignal(signal.SIGTERM)
        signal.alarm(20)
        try:
            started = time.monotonic()
            steam_status.main()
        finally:
            signal.alarm(0)
            signal.signal(signal.SIGTERM, term)
        self.assertGreaterEqual(time.monotonic() - started, 1.0)
        self.assertFalse(os.path.exists(self.status))


if __name__ == "__main__":
    unittest.main()
