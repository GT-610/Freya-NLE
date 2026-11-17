import importlib.util
import json
from pathlib import Path
import shutil
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().parent.parent / "scripts" / "verify-release-contents.py"
spec = importlib.util.spec_from_file_location("verify_release_contents", SCRIPT)
verify_release = importlib.util.module_from_spec(spec)
spec.loader.exec_module(verify_release)


class ReleaseContentsTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        (self.root / "licenses").mkdir()
        for name, source in verify_release.LICENSES.items():
            shutil.copyfile(source, self.root / "licenses" / name)
        self.info = {
            "name": "Freya NLE", "version": "1.2.3", "commit": "abc123",
            "platform": "linux", "architecture": "x86_64", "variant": None,
            "profile": "Release", "ffmpeg": {"source": "ffmpeg-dir", "version": "abc123"},
        }
        self.write_info()

    def write_info(self):
        (self.root / "build-info.json").write_text(json.dumps(self.info), encoding="utf-8")

    def verify(self):
        verify_release.verify(self.root, "1.2.3", "abc123", "linux", "x86_64", None)

    def test_valid_artifact(self):
        self.verify()

    def test_mismatched_metadata_is_rejected(self):
        for key, wrong in (("commit", "other"), ("architecture", "arm64"),
                           ("version", "1.2.4"), ("variant", "archlinux"),
                           ("profile", "Debug")):
            with self.subTest(key=key):
                previous = self.info[key]
                self.info[key] = wrong
                self.write_info()
                with self.assertRaisesRegex(ValueError, key):
                    self.verify()
                self.info[key] = previous

    def test_missing_ffmpeg_version_is_rejected(self):
        self.info["ffmpeg"]["version"] = ""
        self.write_info()
        with self.assertRaisesRegex(ValueError, "FFmpeg version"):
            self.verify()

    def test_missing_or_altered_license_is_rejected(self):
        license_file = self.root / "licenses" / "GPL-3.0.txt"
        license_file.write_text("wrong", encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "GPL-3.0"):
            self.verify()
        license_file.unlink()
        with self.assertRaises(FileNotFoundError):
            self.verify()


if __name__ == "__main__":
    unittest.main()
