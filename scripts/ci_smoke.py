#!/usr/bin/env python3
# Copyright (c) 2026 Hakai Offensive Security.
# SPDX-License-Identifier: GPL-3.0-or-later
"""Build and exercise public samples using native LLVM 22 and the Rust host."""

import argparse
import json
import os
from pathlib import Path
import platform
import re
import shutil
import signal
import subprocess
import sys
import time
import traceback
import xml.etree.ElementTree as ET


ROOT = Path(__file__).resolve().parents[1]
EXE = ".exe" if os.name == "nt" else ""
FLAGS = ["--string-enc", "--shuffle-blocks", "--indirect-branch", "--mba"]
STDOUT = (
    "This is a secret message that should be encrypted.\n"
    "Calculation result: 7\n"
    "Value is large!\n"
    "And it's even.\n"
    "Check passed!\n"
    "FFT bin[0] = (9.0, 0.0)\n"
    "FFT bin[1] = (1.0, 0.0)\n"
).encode()
CANARIES = {
    "test_project": STDOUT.splitlines()[0],
    "test_dll_project": b"lyra-dll: secret message that must be decrypted at DLL_PROCESS_ATTACH time",
}


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def clean_environment(llvm_bin):
    # Do not let a developer's target directory, rustflags, wrappers or prior
    # Lyra invocation bypass interception or clean anything outside the copy.
    env = {key: value for key, value in os.environ.items()
           if not key.upper().startswith(("LYRA_", "CARGO_TARGET_", "CARGO_PROFILE_"))
           and key.upper() not in {"RUSTFLAGS", "RUSTDOCFLAGS", "CARGO_ENCODED_RUSTFLAGS",
                                   "CARGO_ENCODED_RUSTDOCFLAGS", "RUSTC_WRAPPER",
                                   "RUSTC_WORKSPACE_WRAPPER", "CARGO_BUILD_TARGET",
                                   "CARGO_BUILD_RUSTFLAGS", "CARGO_BUILD_RUSTC_WRAPPER",
                                   "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER", "CARGO_BUILD_TARGET_DIR"}}
    env.update(LYRA_LLVM_BIN=str(llvm_bin), CARGO_PROFILE_RELEASE_LTO="off",
               CARGO_PROFILE_RELEASE_CODEGEN_UNITS="1", CARGO_NET_OFFLINE="true",
               CARGO_TERM_COLOR="never")
    return env


class Runner:
    def __init__(self, output, llvm_bin, lyra):
        self.output, self.llvm_bin, self.lyra = output, llvm_bin, lyra
        self.env = clean_environment(llvm_bin)
        self.report = {"schema_version": 1, "versions": {"rustc": None, "llvm": {}, "target": None},
                       "results": []}

    def save_report(self):
        results = self.report["results"]
        (self.output / "report.json").write_text(json.dumps(self.report, indent=2) + "\n", encoding="utf-8")
        suite = ET.Element("testsuite", name="lyra-native", tests=str(len(results)),
                           failures=str(sum(r["status"] == "failed" for r in results)),
                           time=f"{sum(r['seconds'] for r in results):.6f}")
        for result in results:
            case = ET.SubElement(suite, "testcase", name=result["name"], time=f"{result['seconds']:.6f}")
            if result["status"] == "failed":
                ET.SubElement(case, "failure", message=result["error"]).text = result["error"]
            ET.SubElement(case, "system-out").text = result["artifacts"]
        ET.ElementTree(suite).write(self.output / "junit.xml", encoding="utf-8", xml_declaration=True)

    def case(self, name, action):
        directory = self.output / name
        directory.mkdir(parents=True)
        started = time.monotonic()
        result = {"name": name, "status": "passed", "artifacts": str(directory)}
        try:
            action(directory)
        except Exception as error:
            result.update(status="failed", error=f"{type(error).__name__}: {error}")
            (directory / "failure.log").write_text(traceback.format_exc(), encoding="utf-8")
        finally:
            result["seconds"] = time.monotonic() - started
            self.report["results"].append(result)
            self.save_report()
        print(f"{result['status'].upper()}: {name}", flush=True)
        return result["status"] == "passed"

    def run(self, directory, label, argv, cwd=ROOT, timeout=600, capture=False):
        temporary = directory / "tmp"
        temporary.mkdir(exist_ok=True)
        overrides = {key: str(temporary) for key in ("TMPDIR", "TEMP", "TMP")}
        env = self.env | overrides
        # Only log harness-controlled variables; never serialize the host environment.
        relevant = {key: env[key] for key in ("LYRA_LLVM_BIN", "CARGO_PROFILE_RELEASE_LTO",
                    "CARGO_PROFILE_RELEASE_CODEGEN_UNITS", "CARGO_NET_OFFLINE", "TMPDIR", "TEMP", "TMP")}
        argv = [str(arg) for arg in argv]
        metadata = {"argv": argv, "cwd": str(cwd), "env": relevant, "timeout_seconds": timeout}
        command_log = directory / f"{label}.command.json"
        command_log.write_text(json.dumps(metadata, indent=2) + "\n", encoding="utf-8")
        stdout, stderr = directory / f"{label}.stdout.log", directory / f"{label}.stderr.log"
        with stdout.open("wb") as out, stderr.open("wb") as err:
            process = subprocess.Popen(argv, cwd=cwd, env=env, stdin=subprocess.DEVNULL,
                                       stdout=out, stderr=err, start_new_session=os.name != "nt")
            try:
                process.wait(timeout=timeout)
            except subprocess.TimeoutExpired:
                if os.name == "nt":
                    subprocess.run(["taskkill", "/PID", str(process.pid), "/T", "/F"],
                                   stdout=err, stderr=err, timeout=30, check=False)
                else:
                    try:
                        os.killpg(process.pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass  # The entire process group already exited.
                process.wait(timeout=30)
                metadata["timed_out"] = True
            finally:
                metadata["returncode"] = process.poll()
                command_log.write_text(json.dumps(metadata, indent=2) + "\n", encoding="utf-8")
        # Keep both ends of unusually noisy compiler output, not unlimited logs.
        for path in (stdout, stderr):
            if path.stat().st_size > 8 * 1024 * 1024:
                with path.open("rb") as stream:
                    beginning = stream.read(4 * 1024 * 1024)
                    stream.seek(-4 * 1024 * 1024, os.SEEK_END)
                    ending = stream.read()
                path.write_bytes(beginning + b"\n[log truncated]\n" + ending)
        require(not metadata.get("timed_out"), f"{label} exceeded {timeout}s; see {command_log}")
        require(process.returncode == 0, f"{label} exited {process.returncode}; see {stderr}")
        if capture:
            require(stdout.stat().st_size <= 65536 and stderr.stat().st_size <= 65536,
                    f"{label} produced unexpectedly large runtime/version output")
            return stdout.read_bytes(), stderr.read_bytes()

    def preflight(self, directory):
        rustc, _ = self.run(directory, "rustc-version", ["rustc", "-vV"], timeout=30, capture=True)
        rustc = rustc.decode("utf-8")
        versions = self.report["versions"]
        versions["rustc"] = rustc
        host = re.search(r"^host: (\S+)$", rustc, re.MULTILINE)
        require(host is not None, "rustc -vV did not report its host target")
        self.target = versions["target"] = host.group(1)
        rust_llvm = re.search(r"^LLVM version: (\d+)\.", rustc, re.MULTILINE)
        require(rust_llvm is not None and rust_llvm.group(1) == "22", "rustc must emit LLVM 22 IR")
        machine = {"arm64": "aarch64", "amd64": "x86_64"}.get(platform.machine().lower(), platform.machine().lower())
        system = {"Linux": "unknown-linux-gnu", "Darwin": "apple-darwin", "Windows": "pc-windows-msvc"}
        require(platform.system() in system and self.target == f"{machine}-{system[platform.system()]}",
                f"rustc host {self.target} is not the native supported target for {platform.system()}/{machine}")
        for name in ("llc", "clang", "llvm-ar", "opt"):
            tool = self.llvm_bin / (name + EXE)
            require(tool.is_file(), f"missing LLVM tool: {tool}")
            text, _ = self.run(directory, f"{name}-version", [tool, "--version"], timeout=30, capture=True)
            versions["llvm"][name] = text.decode("utf-8", errors="replace")
            match = re.search(r"(?:LLVM|clang) version (\d+)\.", versions["llvm"][name], re.IGNORECASE)
            require(match is not None and match.group(1) == "22", f"{tool} must be LLVM 22")
        for binary in (self.lyra, self.lyra.with_name("lyra_wrapper" + EXE), self.lyra.with_name("lyra_linker" + EXE)):
            require(binary.is_file(), f"missing native binary: {binary}; build cargo --bins first")

    def execute(self, directory, artifact, fixture):
        command = [artifact] if fixture == "test_project" else [sys.executable, ROOT / "scripts/probe_cdylib.py", artifact]
        actual, stderr = self.run(directory, "runtime", command, cwd=directory, timeout=30, capture=True)
        expected = STDOUT if fixture == "test_project" else b"cdylib ABI and loader-time greeting passed\n"
        # Windows Python writes CRLF; accept only that platform's newline encoding.
        if os.name == "nt":
            actual = actual.replace(b"\r\n", b"\n")
        require(stderr == b"", f"runtime wrote unexpected stderr: {stderr!r}")
        require(actual == expected, f"runtime stdout mismatch: expected {expected!r}, got {actual!r}")
        return actual

    def fixture(self, fixture, suite):
        project = self.output / fixture / "project with spaces"
        is_bin = fixture == "test_project"
        artifact_name = "test_project" + EXE if is_bin else (
            "test_dll.dll" if os.name == "nt" else "libtest_dll.dylib" if sys.platform == "darwin" else "libtest_dll.so")
        baseline_stdout = None

        def baseline(directory):
            nonlocal baseline_stdout
            shutil.copytree(ROOT / fixture, project, ignore=shutil.ignore_patterns("target", ".git"))
            self.run(directory, "cargo-baseline", ["cargo", "build", "--locked", "--release", "--target", self.target,
                     *( ["--bin", "test_project"] if is_bin else ["--lib"])], cwd=project)
            artifact = directory / artifact_name
            shutil.copy2(project / "target" / self.target / "release" / artifact_name, artifact)
            require(CANARIES[fixture] in artifact.read_bytes(), "baseline artifact lacks the full plaintext canary")
            baseline_stdout = self.execute(directory, artifact, fixture)

        try:
            if not self.case(f"{fixture}/baseline/seed-0", baseline):
                return
            profiles = [("no-passes", [])]
            if suite == "full":
                profiles += [(flag[2:], [flag]) for flag in FLAGS]
            profiles.append(("all-passes", FLAGS))
            for profile, flags in profiles:
                for seed in ([0] if suite == "smoke" else [0, 1, 42]):
                    def transformed(directory):
                        artifact = directory / artifact_name
                        command = [self.lyra, "--project", project, "--output", artifact, "--target", self.target,
                                   "--crate-type", "bin" if is_bin else "cdylib", "--seed", str(seed), "--keep-temps", *flags]
                        if is_bin:
                            command += ["--bin", "test_project"]
                        self.run(directory, "lyra", command, cwd=project)
                        ir_files = sorted((directory / "tmp").rglob("*.obf.ll"))
                        require(bool(ir_files), "no intercepted obfuscated IR was produced")
                        for index, ir in enumerate(ir_files):
                            self.run(directory, f"opt-verify-{index}", [self.llvm_bin / ("opt" + EXE),
                                     "-passes=verify", "-disable-output", ir], timeout=60)
                        actual = self.execute(directory, artifact, fixture)
                        require(actual == baseline_stdout, "transformed runtime differs from baseline")
                        if "--string-enc" in flags:
                            require(CANARIES[fixture] not in artifact.read_bytes(), "encrypted artifact retains the full plaintext canary")
                        # Successful cases retain logs; failed cases retain their binary and IR.
                        shutil.rmtree(directory / "tmp")
                        artifact.unlink()
                    self.case(f"{fixture}/{profile}/seed-{seed}", transformed)
        finally:
            # Cargo cleans only this disposable copy. Never upload its target cache.
            target = project / "target"
            if target.exists():
                shutil.rmtree(target)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--suite", choices=("smoke", "full"), default="smoke")
    parser.add_argument("--llvm-bin", type=Path)
    parser.add_argument("--lyra", type=Path, default=ROOT / "target/debug" / ("lyra" + EXE))
    parser.add_argument("--output", type=Path, required=True, help="new absolute diagnostic directory")
    args = parser.parse_args()
    if not args.output.is_absolute() or args.output.exists():
        parser.error("--output must be an absolute path that does not yet exist")
    args.output = args.output.resolve()
    if any(args.output.is_relative_to(ROOT / fixture) for fixture in CANARIES):
        parser.error("--output must not be inside a sample source project")
    args.output.mkdir(parents=True)
    configured = args.llvm_bin or os.environ.get("LYRA_LLVM_BIN")
    if not configured and os.environ.get("LLVM_SYS_221_PREFIX"):
        configured = Path(os.environ["LLVM_SYS_221_PREFIX"]) / "bin"
    llvm_bin = Path(configured).resolve() if configured else args.output / "missing-llvm-bin"
    runner = Runner(args.output, llvm_bin, args.lyra.resolve())
    try:
        if runner.case("preflight", runner.preflight):
            for fixture in CANARIES:
                try:
                    runner.fixture(fixture, args.suite)
                except Exception as error:
                    def infrastructure_failure(directory):
                        raise RuntimeError(f"fixture infrastructure failed: {error}") from error
                    runner.case(f"{fixture}/infrastructure/seed-0", infrastructure_failure)
    finally:
        runner.save_report()
    return int(any(result["status"] == "failed" for result in runner.report["results"]))


if __name__ == "__main__":
    sys.exit(main())
