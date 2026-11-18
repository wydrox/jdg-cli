#!/usr/bin/env python3
import json
import os
import signal
import subprocess
import tempfile
import time
import unittest
from pathlib import Path

SCRIPT = Path(__file__).with_name("saldeo-login.js")

# Stub `playwright`: the first launch fails (main Helium profile busy), so the script
# falls back to a temporary `jdg-cli-helium-*` profile; STUB_MODE picks what happens next.
STUB_PLAYWRIGHT = r"""
const fs = require("fs");
const path = require("path");
let launches = 0;
exports.chromium = {
  async launchPersistentContext(dir) {
    launches += 1;
    if (launches === 1) throw new Error("profile in use");
    fs.writeFileSync(path.join(dir, "Cookies"), "saldeo-session");
    const mode = process.env.STUB_MODE;
    const page = {
      async goto() {
        if (mode === "hang") return new Promise(() => {});
        if (mode === "error") throw new Error("navigation failed");
      },
      async waitForLoadState() {},
    };
    return {
      pages: () => [page],
      async newPage() { return page; },
      once() {},
      async close() {
        fs.writeFileSync(process.env.STUB_CLOSE_MARK, fs.existsSync(dir) ? "dir-present" : "dir-gone");
      },
      async storageState(opts) {
        if (opts && opts.path) fs.writeFileSync(opts.path, "{}");
        const cookies = mode === "ok"
          ? [{ name: "X-SALDEO-XSRF-C-TOKEN", value: "x", domain: "saldeo.brainshare.pl", path: "/", expires: -1, secure: true }]
          : [];
        return { cookies };
      },
      request: {
        async post() {
          return { status: () => 200, text: async () => '{"status":"SUCCESS","data":{"resultCollection":[]}}' };
        },
      },
    };
  },
};
"""


def run_node(js):
    return subprocess.run(["node", "-e", js], capture_output=True, text=True)


class SaldeoLoginTests(unittest.TestCase):
    def test_read_login_file_and_unlink(self):
        script = Path(__file__).with_name("saldeo-login.js")
        with tempfile.TemporaryDirectory() as tmp:
            login = Path(tmp) / "login.json"
            login.write_text(json.dumps({"username": "jan", "password": "tajne-haslo"}))
            os.chmod(login, 0o600)
            js = f"""
const {{ readLoginFile }} = require({json.dumps(str(script))});
const data = readLoginFile({json.dumps(str(login))});
if (data.username !== "jan" || data.password !== "tajne-haslo") process.exit(2);
"""
            result = subprocess.run(["node", "-e", js], capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertFalse(login.exists())

    def test_script_does_not_use_argv_for_password(self):
        text = Path(__file__).with_name("saldeo-login.js").read_text()
        self.assertIn("JDG_SALDEO_LOGIN_FILE", text)
        self.assertNotIn("process.argv[", text)
        self.assertNotIn("SALDEO_PASSWORD", text)

    def test_password_login_waits_for_spa_and_is_headed_by_default(self):
        text = Path(__file__).with_name("saldeo-login.js").read_text()
        self.assertIn("waitAndFillLogin", text)
        self.assertIn("SALDEO_AUTH_HEADLESS === \"1\"", text)
        self.assertNotIn("Boolean(login)", text)
        self.assertIn("waitForLoadState", text)

    def test_script_finds_unnamed_username_without_mat_input_ids(self):
        text = SCRIPT.read_text()
        # Current Saldeo login: formcontrolname lives on <ds-input>, the native
        # input is type=text with qa-id="username" (mat-input-N ids are unstable).
        self.assertIn("inPasswordForm", text)
        self.assertIn('getAttribute("qa-id")', text)
        self.assertIn("Log in", text)
        self.assertIn("Zaloguj", text)
        self.assertIn("data-jdg-login", text)
        self.assertNotIn("mat-input-0", text)
        self.assertNotIn("SALDEO_PASSWORD", text)


class SaldeoLoginPureTests(unittest.TestCase):
    def node_json(self, expr):
        js = f"""
const m = require({json.dumps(str(SCRIPT))});
process.stdout.write(JSON.stringify({expr}));
"""
        result = run_node(js)
        self.assertEqual(result.returncode, 0, result.stderr)
        return json.loads(result.stdout)

    def test_cookies_for_url_follow_domain_path_secure_rules(self):
        cookies = [
            {"name": "host", "value": "1", "domain": "saldeo.brainshare.pl", "path": "/", "secure": True, "expires": -1},
            {"name": "parent", "value": "1", "domain": ".brainshare.pl", "path": "/", "secure": False, "expires": -1},
            {"name": "rest", "value": "1", "domain": "saldeo.brainshare.pl", "path": "/rest", "secure": False, "expires": -1},
            {"name": "other", "value": "1", "domain": ".google.com", "path": "/", "secure": False, "expires": -1},
            {"name": "old", "value": "1", "domain": "saldeo.brainshare.pl", "path": "/", "secure": False, "expires": 10},
        ]
        names = lambda url: self.node_json(
            f"m.cookiesForUrl({json.dumps(cookies)}, {json.dumps(url)}, 1000).map((c) => c.name)"
        )
        self.assertEqual(names("https://saldeo.brainshare.pl/rest/client/document/list/search"), ["host", "parent", "rest"])
        self.assertEqual(names("https://cdn.brainshare.pl/f.pdf"), ["parent"])
        self.assertEqual(names("https://files.example.net/f.pdf"), [])
        self.assertEqual(names("http://saldeo.brainshare.pl/restore"), ["parent"])

    def test_session_response_valid(self):
        cases = {
            "ok": [200, '{"status":"SUCCESS","data":{"resultCollection":[]}}'],
            "no_status": [200, '{"data":{"resultCollection":[]}}'],
            "error_status": [200, '{"status":"ERROR","data":{"resultCollection":[]}}'],
            "html": [200, "<html><form>login</form></html>"],
            "unauthorized": [401, '{"status":"SUCCESS","data":{"resultCollection":[]}}'],
        }
        got = self.node_json(
            "Object.fromEntries(Object.entries(" + json.dumps(cases) + ").map(([k, v]) => [k, m.sessionResponseValid(v[0], v[1])]))"
        )
        self.assertEqual(got, {"ok": True, "no_status": True, "error_status": False, "html": False, "unauthorized": False})

    def test_sweep_removes_only_stale_jdg_profiles(self):
        with tempfile.TemporaryDirectory() as tmp:
            stale = Path(tmp) / "jdg-cli-helium-old"
            fresh = Path(tmp) / "jdg-cli-helium-new"
            unrelated = Path(tmp) / "other-old"
            for d in (stale, fresh, unrelated):
                d.mkdir()
            old = time.time() - 7 * 3600
            os.utime(stale, (old, old))
            os.utime(unrelated, (old, old))
            removed = self.node_json(f"m.sweepStaleTempProfiles({json.dumps(tmp)})")
            self.assertEqual(removed, [str(stale)])
            self.assertFalse(stale.exists())
            self.assertTrue(fresh.exists())
            self.assertTrue(unrelated.exists())

    def test_pick_login_controls_on_current_saldeo_form(self):
        """The live panel (probed 2026-11-17) has an unnamed text input, a password,
        a visibility toggle, and a submit button labelled 'Log in'. A decoy text
        input outside that form must not be filled."""
        cases = r"""
(() => {
const field = (extra) => Object.assign({
  tag: "input", type: "text", name: "", id: "", formControlName: "",
  hostFormControlName: "", autocomplete: "off", qaId: "", text: "",
  inPasswordForm: false, visible: true,
}, extra);
const current = [
  field({ index: 0, name: "search", id: "q" }),
  field({ index: 1, id: "mat-input-0", hostFormControlName: "username", qaId: "username", inPasswordForm: true }),
  field({ index: 2, type: "password", id: "mat-input-1", hostFormControlName: "password", qaId: "password", inPasswordForm: true }),
  field({ index: 3, tag: "button", type: "button", text: "visibility", qaId: "password-trailing-icon", inPasswordForm: true }),
  field({ index: 4, tag: "button", type: "submit", text: "Log in", qaId: "login-button", inPasswordForm: true }),
];
const unnamed = current.map((control, index) => index === 1
  ? field({ index, id: "mat-input-0", inPasswordForm: true })
  : control);
const noForm = unnamed.map((control) => Object.assign({}, control, { inPasswordForm: false }));
const legacy = [
  field({ index: 0, name: "username", id: "login" }),
  field({ index: 1, type: "password", name: "password" }),
  field({ index: 2, tag: "button", type: "submit", text: "Zaloguj się" }),
];
const polishButton = [
  field({ index: 0, inPasswordForm: true }),
  field({ index: 1, type: "password", inPasswordForm: true }),
  field({ index: 2, tag: "button", type: "button", text: "visibility", inPasswordForm: true }),
  field({ index: 3, tag: "button", type: "button", text: "Zaloguj się", inPasswordForm: true }),
];
const hiddenFirst = [
  field({ index: 0, type: "hidden", inPasswordForm: true, visible: false }),
  field({ index: 1, inPasswordForm: true }),
  field({ index: 2, type: "password", inPasswordForm: true }),
];
const email = [
  field({ index: 0, type: "email", autocomplete: "username", inPasswordForm: true }),
  field({ index: 1, type: "password", autocomplete: "current-password", inPasswordForm: true }),
];
const earlierSubmit = [
  field({ index: 0, tag: "button", type: "submit", text: "Search" }),
  field({ index: 1, qaId: "username", inPasswordForm: true }),
  field({ index: 2, type: "password", inPasswordForm: true }),
  field({ index: 3, tag: "button", type: "button", text: "visibility", inPasswordForm: true }),
  field({ index: 4, tag: "button", type: "submit", text: "Log in", qaId: "login-button", inPasswordForm: true }),
];
const picked = (controls) => m.pickLoginControls(controls);
return {
  current: picked(current),
  unnamed: picked(unnamed),
  noForm: picked(noForm),
  legacy: picked(legacy),
  polishButton: picked(polishButton),
  hiddenFirst: picked(hiddenFirst),
  email: picked(email),
  earlierSubmit: picked(earlierSubmit),
};
})()
"""
        got = self.node_json(cases)
        for name in ("current", "unnamed", "noForm"):
            self.assertEqual(got[name], {"user": 1, "pass": 2, "submit": 4}, name)
        self.assertEqual(got["legacy"], {"user": 0, "pass": 1, "submit": 2})
        self.assertEqual(got["polishButton"], {"user": 0, "pass": 1, "submit": 3})
        self.assertEqual(got["hiddenFirst"], {"user": 1, "pass": 2, "submit": None})
        self.assertEqual(got["email"], {"user": 0, "pass": 1, "submit": None})
        self.assertEqual(got["earlierSubmit"], {"user": 1, "pass": 2, "submit": 4})

    def test_fill_login_types_saved_credentials_into_unnamed_field(self):
        password = "tajne-haslo"
        js = r"""
const m = require(process.argv[1]);
const controls = [
  { index: 0, tag: "input", type: "text", name: "search", id: "q", formControlName: "", hostFormControlName: "", autocomplete: "off", qaId: "", text: "", inPasswordForm: false, visible: true },
  { index: 1, tag: "input", type: "text", name: "", id: "mat-input-0", formControlName: "", hostFormControlName: "", autocomplete: "off", qaId: "", text: "", inPasswordForm: true, visible: true },
  { index: 2, tag: "input", type: "password", name: "", id: "mat-input-1", formControlName: "", hostFormControlName: "password", autocomplete: "off", qaId: "password", text: "", inPasswordForm: true, visible: true },
  { index: 3, tag: "button", type: "button", name: "", id: "", formControlName: "", hostFormControlName: "password", autocomplete: "", qaId: "password-trailing-icon", text: "visibility", inPasswordForm: true, visible: true },
  { index: 4, tag: "button", type: "submit", name: "", id: "", formControlName: "", hostFormControlName: "", autocomplete: "", qaId: "login-button", text: "Log in", inPasswordForm: true, visible: true },
];
const seen = [];
const fills = [];
const clicks = [];
const presses = [];
const page = {
  async evaluate(_fn, arg) {
    seen.push(arg === undefined ? null : arg);
    return arg === undefined ? controls : true;
  },
  locator(selector) {
    const role = (selector.match(/data-jdg-login="([^"]+)"/) || [])[1] || selector;
    const loc = {
      first() { return loc; },
      async count() {
        return role === "submit" && !(seen[1] && seen[1].submit != null) ? 0 : 1;
      },
      async click() { clicks.push(role); },
      async fill(value) { fills.push([role, value]); },
      async press(key) { presses.push([role, key]); },
    };
    return loc;
  },
};
m.fillLogin(page, "jan", process.argv[2]).then((ok) => {
  process.stdout.write(JSON.stringify({ ok, seen, fills, clicks, presses }));
}).catch((err) => {
  console.error(err && err.stack ? err.stack : String(err));
  process.exit(1);
});
"""
        result = subprocess.run(
            ["node", "-e", js, str(SCRIPT), password],
            capture_output=True,
            text=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        got = json.loads(result.stdout)
        self.assertTrue(got["ok"])
        self.assertEqual(got["seen"][1], {"user": 1, "pass": 2, "submit": 4})
        self.assertEqual(got["fills"], [["user", "jan"], ["pass", password]])
        self.assertIn("submit", got["clicks"])
        self.assertNotIn("password-trailing-icon", got["clicks"])
        self.assertNotIn(password, json.dumps(got["seen"]))
        self.assertNotIn(password, result.stderr)


class SaldeoLoginTempProfileTests(unittest.TestCase):
    """Runs the script as `jdg-cli` does, with a stub playwright, and checks that the
    temporary profile is gone on every exit path."""

    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        root = Path(self._tmp.name)
        self.tmpdir = root / "tmp"
        self.tmpdir.mkdir()
        self.home = root / "home"
        self.home.mkdir()
        stub = root / "node_modules" / "playwright"
        stub.mkdir(parents=True)
        (stub / "index.js").write_text(STUB_PLAYWRIGHT)
        self.close_mark = root / "closed"
        self.out = root / "state.json"
        self.env = {
            "PATH": os.environ.get("PATH", ""),
            "HOME": str(self.home),
            "TMPDIR": str(self.tmpdir),
            "NODE_PATH": str(root / "node_modules"),
            "JDG_SALDEO_STORAGE_STATE": str(self.out),
            "HELIUM_EXECUTABLE": "/nonexistent/helium",
            "STUB_CLOSE_MARK": str(self.close_mark),
        }

    def tearDown(self):
        self._tmp.cleanup()

    def profiles(self):
        return sorted(p.name for p in self.tmpdir.iterdir() if p.name.startswith("jdg-cli-helium-"))

    def run_script(self, mode, timeout_ms="5000", timeout=30):
        env = dict(self.env, STUB_MODE=mode, SALDEO_AUTH_TIMEOUT_MS=timeout_ms)
        return subprocess.run(["node", str(SCRIPT)], env=env, capture_output=True, text=True, timeout=timeout)

    def test_success_removes_temp_profile(self):
        result = self.run_script("ok")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("jdg-cli-helium-", result.stderr)
        self.assertTrue(self.out.exists())
        self.assertEqual(self.close_mark.read_text(), "dir-present")
        self.assertEqual(self.profiles(), [])

    def test_error_removes_temp_profile(self):
        result = self.run_script("error")
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("navigation failed", result.stderr)
        self.assertEqual(self.profiles(), [])

    def test_no_session_until_deadline_removes_temp_profile(self):
        result = self.run_script("nosession", timeout_ms="100")
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertEqual(self.profiles(), [])

    def test_watchdog_timeout_removes_temp_profile(self):
        # Navigation hangs; the watchdog fires at timeout + 10 s, before `jdg-cli` would SIGKILL.
        start = time.monotonic()
        result = self.run_script("hang", timeout_ms="100")
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertLess(time.monotonic() - start, 20)
        self.assertEqual(self.close_mark.read_text(), "dir-present")
        self.assertEqual(self.profiles(), [])

    def test_signal_removes_temp_profile(self):
        for sig, code in ((signal.SIGTERM, 143), (signal.SIGINT, 130), (signal.SIGHUP, 129)):
            env = dict(self.env, STUB_MODE="hang", SALDEO_AUTH_TIMEOUT_MS="60000")
            proc = subprocess.Popen(["node", str(SCRIPT)], env=env, stderr=subprocess.PIPE, text=True)
            try:
                deadline = time.monotonic() + 10
                while not self.profiles() and time.monotonic() < deadline:
                    time.sleep(0.05)
                self.assertTrue(self.profiles(), "temporary profile was not created")
                time.sleep(0.2)
                proc.send_signal(sig)
                _, stderr = proc.communicate(timeout=15)
            finally:
                if proc.poll() is None:
                    proc.kill()
            self.assertEqual(proc.returncode, code, stderr)
            self.assertEqual(self.profiles(), [], sig)


if __name__ == "__main__":
    unittest.main()
