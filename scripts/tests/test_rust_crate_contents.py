import io
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import unittest


class PublicationContentsTests(unittest.TestCase):
    def compare(self, original, publication):
        with tempfile.TemporaryDirectory() as directory:
            paths = []
            for index, body in enumerate((original, publication)):
                path = Path(directory) / f"{index}.crate"
                with tarfile.open(path, "w:gz") as archive:
                    entry = tarfile.TarInfo(
                        "trellis-runtime-1.0.0/generated/web/index.html"
                    )
                    entry.size = len(body)
                    entry.mode = 0o644
                    # Recompression metadata is not application content.
                    entry.mtime = index
                    archive.addfile(entry, io.BytesIO(body))
                paths.append(str(path))
            return subprocess.run(
                [
                    sys.executable,
                    str(Path(__file__).parents[1] / "verify-rust-crate-contents.py"),
                    *paths,
                ],
                capture_output=True,
                text=True,
            )

    def test_identical_browser_content_survives_repackaging(self):
        result = self.compare(
            b"<html>accepted app</html>", b"<html>accepted app</html>"
        )
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_changed_browser_content_blocks_publication(self):
        result = self.compare(
            b"<html>accepted app</html>", b"<html>different app</html>"
        )
        self.assertNotEqual(result.returncode, 0, result.stdout)


if __name__ == "__main__":
    unittest.main()
