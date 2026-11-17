import os
import stat
import subprocess
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent / "migrate-from-lab.sh"


class MigrateFromLabTests(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.tmp = Path(self._tmp.name)
        self.home = self.tmp / "home"
        self.root = self.tmp / "root"
        self.old_config = self.home / ".config/lab"
        self.new_config = self.home / ".config/jdg-cli"
        self.old_config.mkdir(parents=True)
        self.root.mkdir()
        (self.old_config / "env").write_text(
            f"LAB_ROOT='{self.root}'\n"
            f"GOOGLE_CLIENT_SECRET_PATH='{self.home}/.config/lab/client.json'\n"
            "export LAB_OCR_MODE=off\n"
            "OTHER=lab\n"
        )
        (self.old_config / ".env").write_text("KSEF_TOKEN='secret-value'\nLAB_THEME=dark\n")
        for path in (self.old_config / "env", self.old_config / ".env"):
            path.chmod(0o600)
        (self.root / "lab.sqlite").write_text("db")
        (self.root / "lab.sqlite-wal").write_text("wal")
        logs = self.home / "Library/Logs/lab"
        logs.mkdir(parents=True)
        (logs / "lab.log").write_text("log")

    def tearDown(self):
        self._tmp.cleanup()

    def run_script(self, *args):
        env = {
            "HOME": str(self.home),
            "PATH": "/usr/bin:/bin:/usr/sbin:/sbin",
            "JDG_MIGRATE_CHECKOUT": str(self.tmp / "checkout"),
        }
        return subprocess.run(
            ["bash", str(SCRIPT), *args], env=env, capture_output=True, text=True
        )

    def test_moves_config_db_and_logs_and_renames_keys(self):
        result = self.run_script()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(self.old_config.exists())
        env = (self.new_config / "env").read_text()
        self.assertIn(f"JDG_ROOT='{self.root}'", env)
        self.assertIn(f"'{self.home}/.config/jdg-cli/client.json'", env)
        self.assertIn("export JDG_OCR_MODE=off", env)
        self.assertIn("OTHER=lab", env)
        self.assertNotIn("LAB_", env)
        dotenv = self.new_config / ".env"
        self.assertEqual(dotenv.read_text(), "KSEF_TOKEN='secret-value'\nJDG_THEME=dark\n")
        self.assertEqual(stat.S_IMODE(dotenv.stat().st_mode), 0o600)
        self.assertEqual((self.root / "jdg-cli.sqlite").read_text(), "db")
        self.assertEqual((self.root / "jdg-cli.sqlite-wal").read_text(), "wal")
        self.assertFalse((self.root / "lab.sqlite").exists())
        self.assertEqual((self.home / "Library/Logs/jdg-cli/jdg-cli.log").read_text(), "log")
        self.assertNotIn("secret-value", result.stdout + result.stderr)

        again = self.run_script()
        self.assertEqual(again.returncode, 0, again.stderr)
        self.assertIn("done: 0 change(s)", again.stdout)

    def test_dry_run_changes_nothing(self):
        result = self.run_script("--dry-run")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue((self.old_config / "env").read_text().startswith("LAB_ROOT="))
        self.assertTrue((self.root / "lab.sqlite").exists())
        self.assertFalse(self.new_config.exists())
        self.assertIn("would: mv", result.stdout)

    def test_existing_targets_are_never_overwritten(self):
        self.new_config.mkdir(parents=True)
        (self.root / "jdg-cli.sqlite").write_text("new")
        result = self.run_script(str(self.root))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue((self.old_config / "env").exists())
        self.assertEqual((self.root / "lab.sqlite").read_text(), "db")
        self.assertEqual((self.root / "jdg-cli.sqlite").read_text(), "new")
        self.assertIn("already exists", result.stderr)

    def test_env_symlink_into_old_config_is_repointed(self):
        os.symlink(self.old_config / ".env", self.root / ".env")
        result = self.run_script(str(self.root))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(os.readlink(self.root / ".env"), str(self.new_config / ".env"))
        self.assertIn("JDG_THEME=dark", (self.root / ".env").read_text())


if __name__ == "__main__":
    unittest.main()
