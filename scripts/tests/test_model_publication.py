import io
import json
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import unittest


class ModelPublicationTests(unittest.TestCase):
    def compare(self, old_version, new_version, old_digest, new_digest, old_abi=1, new_abi=1):
        with tempfile.TemporaryDirectory() as directory:
            paths = []
            for index, (version, digest, abi) in enumerate(((old_version, old_digest, old_abi), (new_version, new_digest, new_abi))):
                path = Path(directory) / f"{index}.crate"
                prefix = f"trellis-models-{version}"
                with tarfile.open(path, "w:gz") as archive:
                    files = {
                        "Cargo.toml": f'[package]\nname = "trellis-models"\nversion = "{version}"\n'.encode(),
                        "generated/models.json": json.dumps({"format": "trellis.rust-models.v1", "root": "trellis", "rustAbi": abi, "packages": [{"name": "trellis", "version": "0.100.0", "digest": digest}]}).encode(),
                    }
                    for name, body in files.items():
                        entry = tarfile.TarInfo(f"{prefix}/{name}")
                        entry.size = len(body)
                        archive.addfile(entry, io.BytesIO(body))
                paths.append(str(path))
            return subprocess.run([sys.executable, str(Path(__file__).parents[1] / "verify-model-publication.py"), paths[1], "--previous", paths[0]], capture_output=True, text=True)

    def test_model_patch_preserves_generated_bindings(self):
        result = self.compare("0.100.0-rc.1", "0.100.0-rc.2", "a", "a")
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_changed_native_snapshot_requires_new_compatibility_line(self):
        result = self.compare("0.100.0", "0.100.1", "a", "b")
        self.assertNotEqual(result.returncode, 0, result.stdout)

    def test_changed_rust_representation_requires_new_compatibility_line(self):
        result = self.compare("1.0.0", "1.1.0", "a", "a", new_abi=2)
        self.assertNotEqual(result.returncode, 0, result.stdout)

    def test_new_snapshot_can_publish_on_distinct_compatibility_line(self):
        result = self.compare("0.100.0", "0.101.0", "a", "b", new_abi=2)
        self.assertEqual(result.returncode, 0, result.stderr)


if __name__ == "__main__":
    unittest.main()
