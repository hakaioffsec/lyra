"""Filesystem regressions for the Unix SDK overlay."""
import os
from pathlib import Path
import stat
import tempfile
import unittest

from build_llvm import clone_sdk


@unittest.skipIf(os.name == "nt", "Unix overlay; Windows uses private copies")
class SdkOverlayTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        root = Path(self.temporary.name).resolve()
        self.base = root / "stock"
        self.prefix = root / "private"
        self.prefix.mkdir()
        for directory in ("bin", "lib", "include", "share"):
            (self.base / directory).mkdir(parents=True)
        for name in ("bin/llvm-config", "bin/llc", "lib/LLVMCodeGen.a", "lib/Other.a"):
            (self.base / name).write_bytes(b"stock contents")
        (self.base / "bin/llvm-config").chmod(0o755)
        (self.base / "include/header.h").write_text("header", encoding="utf-8")

    def test_replacing_outputs_cannot_modify_stock_sdk(self):
        clone_sdk(self.base, self.prefix)
        self.assertTrue(os.path.samefile(self.base / "lib/Other.a", self.prefix / "lib/Other.a"))
        self.assertTrue(os.path.samefile(self.base / "include/header.h", self.prefix / "include/header.h"))
        config = self.prefix / "bin/llvm-config"
        self.assertFalse(config.is_symlink())
        self.assertEqual(stat.S_IMODE(config.stat().st_mode), 0o755)
        config.write_bytes(b"private config")
        self.assertEqual((self.base / "bin/llvm-config").read_bytes(), b"stock contents")
        for name in ("bin/llc", "lib/LLVMCodeGen.a"):
            replacement = self.prefix / "replacement"
            replacement.write_bytes(b"patched contents")
            replacement.replace(self.prefix / name)
            self.assertEqual((self.prefix / name).read_bytes(), b"patched contents")
            self.assertEqual((self.base / name).read_bytes(), b"stock contents")
        licenses = self.prefix / "share/lyra-llvm-source-licenses"
        licenses.mkdir()
        (licenses / "LICENSE").write_text("notice", encoding="utf-8")
        self.assertFalse((self.base / "share/lyra-llvm-source-licenses").exists())

    def test_cyclic_provider_alias_is_not_expanded(self):
        (self.base / "build").symlink_to(self.base, target_is_directory=True)
        clone_sdk(self.base, self.prefix)
        self.assertTrue((self.prefix / "build").is_symlink())
        self.assertEqual((self.prefix / "build").resolve(), self.base)
        self.assertEqual((self.prefix / "include/header.h").read_text(encoding="utf-8"), "header")

    def test_refuses_writable_container_linked_to_stock(self):
        (self.prefix / "bin").symlink_to(self.base / "bin", target_is_directory=True)
        with self.assertRaisesRegex(RuntimeError, "real directory"):
            clone_sdk(self.base, self.prefix)
        self.assertEqual((self.base / "bin/llvm-config").read_bytes(), b"stock contents")


if __name__ == "__main__":
    unittest.main()
