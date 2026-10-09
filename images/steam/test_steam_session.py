"""Tests for steam-session: `python3 -m unittest discover -s images/steam`.

steam-run is a stand-in that logs which session it was asked for and then runs
a script the test wrote for that session (steam-NAME in the bin dir); no real
Steam, gamescope or XFCE is involved. Waits are shortened through CHA_STEAM_*.
"""

import os
import signal
import subprocess
import tempfile
import time
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
SCRIPT = os.path.join(HERE, "steam-session")

# What steam-run does here: record the session, the request file and the
# overlay file as they are when it starts, then run the session's script (so
# the exit code is the script's).
FAKE_RUN = """#!/bin/sh
# usage: steam-run --name NAME steam-MODE
echo "$CHA_STEAM_SESSION request=$(cat "$XDG_RUNTIME_DIR/steam-session" 2>/dev/null || echo none) overlay=$(cat "$MANGOHUD_CONFIGFILE" 2>/dev/null || echo none) name=$2 pid=$(test -e "$HOME/.steam/steam.pid" && echo stale || echo clean)" >> "$XDG_RUNTIME_DIR/sessions"
exec "$3"
"""


class Rig(tempfile.TemporaryDirectory):
    def __enter__(self):
        root = super().__enter__()
        self.home = os.path.join(root, "home")
        self.run = os.path.join(root, "run")
        self.bin = os.path.join(root, "bin")
        self.proc = os.path.join(root, "proc")
        for d in (self.home, self.run, self.bin, self.proc, os.path.join(self.home, ".steam")):
            os.makedirs(d, exist_ok=True)
        self.script("steam-run", FAKE_RUN)
        return self

    def script(self, name, body):
        path = os.path.join(self.bin, name)
        with open(path, "w") as f:
            f.write(body if body.startswith("#!") else "#!/bin/sh\n" + body)
        os.chmod(path, 0o755)

    def env(self, **extra):
        env = {
            "PATH": f"{self.bin}:{os.environ['PATH']}",
            "HOME": self.home,
            "XDG_RUNTIME_DIR": self.run,
            "MANGOHUD_CONFIGFILE": os.path.join(self.run, "mangohud.conf"),
            "CHA_PROC": self.proc,
            "CHA_STEAM_GROUP_WAIT": "1",
            "CHA_STEAM_CLIENT_WAIT": "1",
            "CHA_STEAM_QUICK": "5",
        }
        env.update(extra)
        return env

    def start(self, **extra):
        return subprocess.Popen(
            [SCRIPT], env=self.env(**extra), stdout=subprocess.PIPE,
            stderr=subprocess.PIPE, text=True,
        )

    def run_it(self, **extra):
        proc = self.start(**extra)
        out, err = proc.communicate(timeout=60)
        return proc.returncode, err

    def sessions(self):
        try:
            with open(os.path.join(self.run, "sessions")) as f:
                return f.read().splitlines()
        except FileNotFoundError:
            return []

    def request(self, mode):
        # What steamos-session-select leaves.
        return f'echo {mode} > "$XDG_RUNTIME_DIR/steam-session"\n'


class SteamSession(unittest.TestCase):
    def test_gaming_to_desktop_and_back_then_the_end(self):
        with Rig() as rig:
            rig.script("steam-gaming", f'''
n=$(cat "$XDG_RUNTIME_DIR/count" 2>/dev/null || echo 0)
echo $((n+1)) > "$XDG_RUNTIME_DIR/count"
if [ "$n" = 0 ]; then {rig.request("desktop")} fi
exit 0
''')
            # The user logs out: no request.
            rig.script("steam-desktop", "sleep 0.1\nexit 0\n")
            code, err = rig.run_it(CHA_STEAM_QUICK="0")
            self.assertEqual(code, 0, err)
            names = [line.split()[0] for line in rig.sessions()]
            self.assertEqual(names, ["gaming", "desktop", "gaming"])
            self.assertIn("name=gamescope", rig.sessions()[0])
            self.assertIn("name=desktop", rig.sessions()[1])
            self.assertIn("switching to desktop", err)
            self.assertIn("switching to gaming", err)

    def test_a_request_for_gaming_from_the_desktop(self):
        with Rig() as rig:
            rig.script("steam-gaming", f'''
n=$(cat "$XDG_RUNTIME_DIR/count" 2>/dev/null || echo 0)
echo $((n+1)) > "$XDG_RUNTIME_DIR/count"
if [ "$n" = 0 ]; then {rig.request("desktop")} fi
exit 0
''')
            rig.script("steam-desktop", rig.request("gaming") + "exit 0\n")
            code, err = rig.run_it()
            self.assertEqual(code, 0, err)
            self.assertEqual(
                [line.split()[0] for line in rig.sessions()],
                ["gaming", "desktop", "gaming"],
            )

    def test_a_crash_code_is_passed_on_and_nothing_follows(self):
        with Rig() as rig:
            rig.script("steam-gaming", "exit 134\n")
            code, err = rig.run_it()
            self.assertEqual(code, 134)
            self.assertEqual(len(rig.sessions()), 1)
        with Rig() as rig:
            rig.script("steam-gaming", rig.request("desktop") + "exit 0\n")
            rig.script("steam-desktop", "exit 3\n")
            code, err = rig.run_it()
            self.assertEqual(code, 3)
            self.assertEqual(len(rig.sessions()), 2)

    def test_an_exit_request_ends_it_from_either_mode(self):
        with Rig() as rig:
            rig.script("steam-gaming", rig.request("exit") + "exit 0\n")
            code, err = rig.run_it()
            self.assertEqual(code, 0)
            self.assertEqual(len(rig.sessions()), 1)
        with Rig() as rig:
            rig.script("steam-gaming", rig.request("desktop") + "exit 0\n")
            rig.script("steam-desktop", rig.request("exit") + "exit 0\n")
            code, err = rig.run_it()
            self.assertEqual(code, 0)
            self.assertEqual(len(rig.sessions()), 2)

    def test_a_failing_session_ignores_a_request(self):
        with Rig() as rig:
            rig.script("steam-gaming", rig.request("desktop") + "exit 1\n")
            code, err = rig.run_it()
            self.assertEqual(code, 1)
            self.assertEqual(len(rig.sessions()), 1)

    def test_a_stop_is_passed_on_and_starts_nothing_more(self):
        with Rig() as rig:
            rig.script(
                "steam-gaming",
                'trap \'echo got-term > "$XDG_RUNTIME_DIR/term"; exit 0\' TERM\n'
                'touch "$XDG_RUNTIME_DIR/up"\nwhile :; do sleep 0.1; done\n',
            )
            proc = rig.start()
            deadline = time.monotonic() + 20
            while not os.path.exists(os.path.join(rig.run, "up")):
                self.assertLess(time.monotonic(), deadline)
                time.sleep(0.05)
            proc.send_signal(signal.SIGTERM)
            proc.communicate(timeout=30)
            self.assertEqual(proc.returncode, 0)
            self.assertTrue(os.path.exists(os.path.join(rig.run, "term")))
            self.assertEqual(len(rig.sessions()), 1)

    def test_a_stop_during_a_desktop_doesnt_return_to_gaming(self):
        with Rig() as rig:
            rig.script("steam-gaming", rig.request("desktop") + "exit 0\n")
            rig.script(
                "steam-desktop",
                'trap \'exit 0\' TERM\ntouch "$XDG_RUNTIME_DIR/up"\nwhile :; do sleep 0.1; done\n',
            )
            proc = rig.start()
            deadline = time.monotonic() + 20
            while not os.path.exists(os.path.join(rig.run, "up")):
                self.assertLess(time.monotonic(), deadline)
                time.sleep(0.05)
            proc.send_signal(signal.SIGTERM)
            proc.communicate(timeout=30)
            self.assertEqual(proc.returncode, 0)
            self.assertEqual(
                [line.split()[0] for line in rig.sessions()], ["gaming", "desktop"]
            )

    def test_the_overlay_file_is_written_in_gaming_and_gone_in_the_desktop(self):
        with Rig() as rig:
            overlay = os.path.join(rig.run, "mangohud.conf")
            # The streamer left it on in the last container.
            with open(overlay, "w") as f:
                f.write("mangoapp_steam\npreset=3\n")
            rig.script("steam-gaming", f'''
n=$(cat "$XDG_RUNTIME_DIR/count" 2>/dev/null || echo 0)
echo $((n+1)) > "$XDG_RUNTIME_DIR/count"
if [ "$n" = 0 ]; then {rig.request("desktop")} fi
exit 0
''')
            rig.script("steam-desktop", "exit 0\n")
            code, err = rig.run_it(CHA_STEAM_QUICK="0")
            self.assertEqual(code, 0, err)
            lines = rig.sessions()
            self.assertIn("overlay=no_display", lines[0])
            self.assertIn("overlay=none", lines[1])
            self.assertIn("overlay=no_display", lines[2])

    def test_the_request_is_cleared_before_each_session(self):
        with Rig() as rig:
            # A request left by the last container.
            with open(os.path.join(rig.run, "steam-session"), "w") as f:
                f.write("desktop\n")
            rig.script("steam-gaming", "exit 0\n")
            code, err = rig.run_it()
            # Not obeyed: gaming mode ended without a request of its own.
            self.assertEqual(code, 0, err)
            self.assertEqual(len(rig.sessions()), 1)
            self.assertIn("request=none", rig.sessions()[0])

    def test_the_session_is_told_its_mode_and_stale_steam_files_go(self):
        with Rig() as rig:
            for name in ("steam.pid", "steam.pipe"):
                open(os.path.join(rig.home, ".steam", name), "w").close()
            rig.script("steam-gaming", "exit 0\n")
            code, err = rig.run_it()
            self.assertEqual(code, 0, err)
            self.assertTrue(rig.sessions()[0].startswith("gaming "))
            self.assertIn("pid=clean", rig.sessions()[0])
            self.assertFalse(os.path.exists(os.path.join(rig.home, ".steam", "steam.pipe")))

    def test_leftover_processes_of_the_old_session_are_killed(self):
        with Rig() as rig:
            marker = os.path.join(rig.run, "leftover.pid")
            # The session starts a stubborn helper in its group, then ends.
            rig.script("steam-gaming", f'''
sh -c 'trap "" TERM; echo $$ > "{marker}"; while :; do sleep 0.1; done' &
while [ ! -s "{marker}" ]; do sleep 0.05; done
exit 0
''')
            code, err = rig.run_it()
            self.assertEqual(code, 0, err)
            self.assertIn("killing it", err)
            with open(marker) as f:
                pid = int(f.read())
            deadline = time.monotonic() + 5
            while time.monotonic() < deadline:
                try:
                    os.kill(pid, 0)
                except ProcessLookupError:
                    break
                time.sleep(0.05)
            else:
                self.fail("the helper survived")

    def test_a_leftover_steam_client_is_waited_for_then_killed(self):
        with Rig() as rig:
            victim = subprocess.Popen(["sleep", "60"], start_new_session=True)
            try:
                entry = os.path.join(rig.proc, str(victim.pid))
                os.makedirs(entry)
                with open(os.path.join(entry, "comm"), "w") as f:
                    f.write("steam\n")
                rig.script("steam-gaming", "exit 0\n")
                code, err = rig.run_it()
                self.assertEqual(code, 0, err)
                self.assertIn("waiting for the Steam client", err)
                self.assertIn("didn't end, killing it", err)
                # The fake /proc entry stays, but the process is gone.
                self.assertIsNotNone(victim.wait(timeout=5))
            finally:
                victim.kill()
                victim.wait()

    def test_a_desktop_that_dies_at_once_twice_in_a_row_is_a_loop(self):
        with Rig() as rig:
            rig.script("steam-gaming", f'''
n=$(cat "$XDG_RUNTIME_DIR/count" 2>/dev/null || echo 0)
echo $((n+1)) > "$XDG_RUNTIME_DIR/count"
{rig.request("desktop")}
exit 0
''')
            rig.script("steam-desktop", "exit 0\n")
            code, err = rig.run_it()
            self.assertEqual(code, 1)
            self.assertIn("twice in a row", err)
            names = [line.split()[0] for line in rig.sessions()]
            self.assertEqual(names, ["gaming", "desktop", "gaming", "desktop"])

    def test_a_desktop_that_lasted_resets_the_count(self):
        with Rig() as rig:
            rig.script("steam-gaming", f'''
n=$(cat "$XDG_RUNTIME_DIR/count" 2>/dev/null || echo 0)
echo $((n+1)) > "$XDG_RUNTIME_DIR/count"
if [ "$n" -lt 3 ]; then {rig.request("desktop")} fi
exit 0
''')
            rig.script("steam-desktop", "sleep 0.3\nexit 0\n")
            # 0.2 s counts as long: three desktops in a row are fine.
            code, err = rig.run_it(CHA_STEAM_QUICK="0.2")
            self.assertEqual(code, 0, err)
            names = [line.split()[0] for line in rig.sessions()]
            self.assertEqual(names.count("desktop"), 3)


if __name__ == "__main__":
    unittest.main()
