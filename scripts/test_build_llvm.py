"""Private SDK configuration and filesystem regressions."""
import os
from pathlib import Path
import stat
import subprocess
import tempfile
import unittest

from build_llvm import clone_sdk, replacement_flags, write_msvc_response


class SdkConfigurationTests(unittest.TestCase):
    def test_noble_relwithdebinfo_preserves_abi_rtti_and_eh_flags(self):
        # LLVM 23.1.6 noble: RelWithDebInfo, assertions OFF, RTTI ON, EH OFF.
        stock = ["-I/sdk/include", "-std=c++17", "-fno-exceptions",
                 "-D_GNU_SOURCE", "-D_GLIBCXX_USE_CXX11_ABI=1",
                 "-D__STDC_CONSTANT_MACROS", "-D__STDC_FORMAT_MACROS",
                 "-D__STDC_LIMIT_MACROS"]
        flags = replacement_flags({"build_mode": "RelWithDebInfo", "assertions": "OFF",
                                   "cxxflags": " ".join(stock)}, False)
        self.assertEqual(flags[:len(stock)], stock)
        self.assertIn("-DNDEBUG", flags)
        self.assertNotIn("-fno-rtti", flags)
        self.assertNotIn("-fexceptions", flags)
        self.assertIn("-O2", flags)
        self.assertIn("-g", flags)

    def test_assertion_mode_controls_ndebug_for_each_optimized_mode(self):
        for mode in ("Release", "RelWithDebInfo", "MinSizeRel"):
            for windows in (False, True):
                for assertions in ("ON", "OFF"):
                    with self.subTest(mode=mode, windows=windows, assertions=assertions):
                        flags = replacement_flags({"build_mode": mode, "assertions": assertions,
                                                   "cxxflags": ""}, windows)
                        prefix = "/" if windows else "-"
                        self.assertIn(prefix + ("UNDEBUG" if assertions == "ON" else "DNDEBUG"), flags)
                        self.assertNotIn(prefix + ("DNDEBUG" if assertions == "ON" else "UNDEBUG"), flags)

    def test_preserves_reported_rtti_and_exception_configuration(self):
        stock = "-std=c++17 -fno-rtti -fexceptions -D_GLIBCXX_USE_CXX11_ABI=0"
        flags = replacement_flags({"build_mode": "Release", "assertions": "ON",
                                   "cxxflags": stock}, False)
        self.assertEqual(flags[:4], stock.split())
        self.assertNotIn("-fno-exceptions", flags)
        self.assertNotIn("-frtti", flags)

    def test_unsupported_configuration_has_actionable_error(self):
        for mode, assertions in (("Debug", "ON"), ("", "OFF"), ("Release", "UNKNOWN")):
            with self.subTest(mode=mode, assertions=assertions):
                with self.assertRaisesRegex(RuntimeError, "omit --base-prefix"):
                    replacement_flags({"build_mode": mode, "assertions": assertions,
                                       "cxxflags": ""}, False)

    def test_msvc_response_keeps_linker_arguments_on_driver_line(self):
        command = [Path("C:/build dir/llc.obj"), "/MT", "/FeC:/build dir/llc.exe",
                   "/link", "-LIBPATH:C:/private SDK/lib",
                   Path("C:/private SDK/lib/LLVMCodeGen.lib"), "zs.lib"]
        with tempfile.TemporaryDirectory() as directory:
            response = Path(directory) / "link.rsp"
            write_msvc_response(response, command)
            text = response.read_text(encoding="utf-8")
        self.assertEqual(text, subprocess.list2cmdline(list(map(str, command))) + "\n")
        self.assertEqual(len(text.splitlines()), 1)
        self.assertIn('/link "-LIBPATH:C:/private SDK/lib"', text)
        self.assertTrue(text.endswith("zs.lib\n"))


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
