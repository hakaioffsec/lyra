"""Runtime-library validation for MSVC COFF directive reports."""
import unittest

from windows_llvm_sdk import is_release_static_crt


class CrtDirectiveTests(unittest.TestCase):
    def test_accepts_static_release_directives_from_c_and_cpp(self):
        for directives in (
            'Directive(s): /DEFAULTLIB:"LIBCMT" /DEFAULTLIB:"OLDNAMES"',
            '/DEFAULTLIB:libcmt.lib /DEFAULTLIB:oldnames.lib',
            '/FAILIFMISMATCH:"RuntimeLibrary=MT_StaticRelease"',
        ):
            with self.subTest(directives=directives):
                self.assertTrue(is_release_static_crt(directives))

    def test_rejects_missing_debug_dynamic_and_mixed_runtimes(self):
        for directives in (
            '', '/DEFAULTLIB:"MSVCRT"', '/DEFAULTLIB:"LIBCMTD"',
            '/DEFAULTLIB:LIBCMT_EXTRA',
            '/DEFAULTLIB:LIBCMT /DEFAULTLIB:"MSVCRT.lib"',
            '/DEFAULTLIB:"LIBCMT" /DEFAULTLIB:"MSVCRTD"',
            '/DEFAULTLIB:"LIBCMT" /FAILIFMISMATCH:"RuntimeLibrary=MD_DynamicRelease"',
            '/DEFAULTLIB:"LIBCMT" /FAILIFMISMATCH:"RuntimeLibrary=MTd_StaticDebug"',
        ):
            with self.subTest(directives=directives):
                self.assertFalse(is_release_static_crt(directives))


if __name__ == "__main__":
    unittest.main()
