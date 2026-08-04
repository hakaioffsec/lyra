// Demo C rule: literal-string detection over the test_project binary.
//
// Verified 2026-08-01 against LLVM 22.1.8 / rustc 1.97.1 on
// x86_64-unknown-linux-gnu:
//
//   yara tests/demo.yar <plain>   ->  demo_secret <plain>   (match)
//   yara tests/demo.yar <obf>     ->  (no output)           (no match)
//
// The obfuscated build is produced with `--string-enc`, which XORs the
// literal with a per-byte key and decrypts it in place from a
// @llvm.global_ctors entry before main. The plaintext is therefore
// absent from the file on disk, so a literal-string rule cannot match,
// even though the program still prints the string at runtime.

rule demo_secret {
  strings: $s = "This is a secret message"
  condition: $s
}
