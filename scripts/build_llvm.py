#!/usr/bin/env python3
"""Prepare a private LLVM 23 SDK.

With --base-prefix, rebuild CodeGenPrepare and statically relink upstream llc.
Only llc and the static LLVM libraries are patched; copied clang/opt codegen
internals and shared LLVM libraries are unchanged. Consumers must link statically.
Without --base-prefix, build the complete SDK from the pinned source archive.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shlex
import shutil
import subprocess
import sys
import tarfile
import urllib.request

VERSION = "23.1.6"
ARCHIVE = f"llvm-project-{VERSION}.src.tar.xz"
URL = f"https://github.com/llvm/llvm-project/releases/download/llvmorg-{VERSION}/{ARCHIVE}"
SHA256 = "d3b86ab29cfb4445a6652e5c2642ccc75e04e38084dde3701d323320f35a31da"
SIZE = 179172364
ROOT = Path(__file__).resolve().parent.parent
PATCH = ROOT / "patches/llvm23-static-alloca-unmerge.patch"
MARKER = ".lyra-llvm-sdk.json"
OPTIONS = [
    "-DCMAKE_BUILD_TYPE=Release",
    "-DLLVM_ENABLE_PROJECTS=clang",
    "-DLLVM_TARGETS_TO_BUILD=X86;ARM;AArch64;RISCV;BPF;NVPTX;WebAssembly",
    "-DLLVM_ENABLE_ASSERTIONS=OFF",
    "-DLLVM_INCLUDE_TESTS=OFF",
    "-DLLVM_INCLUDE_BENCHMARKS=OFF",
    "-DLLVM_INCLUDE_EXAMPLES=OFF",
    "-DLLVM_ENABLE_BINDINGS=OFF",
    "-DLLVM_ENABLE_LIBXML2=OFF",
    "-DLLVM_ENABLE_ZSTD=OFF",
    "-DLLVM_ENABLE_ZLIB=OFF",
    "-DLLVM_ENABLE_TERMINFO=OFF",
    "-DLLVM_ENABLE_LIBEDIT=OFF",
    "-DLLVM_ENABLE_CURL=OFF",
    "-DLLVM_BUILD_LLVM_DYLIB=OFF",
    "-DLLVM_LINK_LLVM_DYLIB=OFF",
    "-DBUILD_SHARED_LIBS=OFF",
    "-DCMAKE_MSVC_RUNTIME_LIBRARY=MultiThreaded",
    "-DLLVM_USE_CRT_RELEASE=MT",
]


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def save(path, value):
    temporary = path.with_suffix(".tmp")
    temporary.write_text(json.dumps(value, indent=2) + "\n", encoding="utf-8")
    temporary.replace(path)


def claim(path, identity):
    if path.is_symlink():
        raise RuntimeError(f"Refusing symlink directory: {path}")
    marker = path / MARKER
    if marker.exists():
        state = json.loads(marker.read_text(encoding="utf-8"))
        if state.get("identity") != identity:
            raise RuntimeError(f"SDK identity mismatch: {path}; choose a new directory")
        return state
    if path.exists() and any(path.iterdir()):
        raise RuntimeError(f"Refusing unowned nonempty directory: {path}")
    path.mkdir(parents=True, exist_ok=True)
    state = {"identity": identity, "complete": False}
    save(marker, state)
    return state


def run(command, log, timeout=None):
    print("Running: " + " ".join(map(str, command)), flush=True)
    with log.open("a", encoding="utf-8") as stream:
        stream.write("\n$ " + " ".join(map(str, command)) + "\n")
        stream.flush()
        try:
            subprocess.run(list(map(str, command)), stdout=stream,
                           stderr=subprocess.STDOUT, check=True, timeout=timeout)
        except (subprocess.CalledProcessError, subprocess.TimeoutExpired) as error:
            raise RuntimeError(f"Command failed: {error}; see {log}") from error


def regress(prefix, logs):
    suffix = ".exe" if os.name == "nt" else ""
    tool = lambda name: prefix / "bin" / (name + suffix)
    required = ["llc", "opt", "clang", "clang++", "llvm-config", "llvm-ar",
                "llvm-as", "llvm-dis", "llvm-nm", "llvm-strip", "llvm-objdump"]
    for name in required:
        if not tool(name).is_file():
            raise RuntimeError(f"Installed SDK lacks {name}")
    version = subprocess.check_output([tool("llvm-config"), "--version"],
                                      text=True, timeout=30).strip()
    if version != VERSION:
        raise RuntimeError(f"Unexpected installed LLVM version: {version}")
    for name in ("static_alloca", "nonstatic_control"):
        source = ROOT / f"tests/llvm23_{name}.ir"
        run([tool("opt"), "-passes=verify", "-disable-output", source],
            logs / "regression.log", 30)
        run([tool("llc"), "-O2", "-mtriple=x86_64-unknown-linux-gnu",
             "-verify-machineinstrs", source, "-o", logs / f"{name}.s"],
            logs / "regression.log", 30)
    prepared = logs / "nonstatic-control.mir"
    run([tool("llc"), "-O2", "-mtriple=x86_64-unknown-linux-gnu",
         "-stop-after=codegenprepare", ROOT / "tests/llvm23_nonstatic_control.ir",
         "-o", prepared], logs / "regression.log", 30)
    text = prepared.read_text(encoding="utf-8")
    if not re.search(r"getelementptr(?: inbounds)? i8, ptr %p8, i64 16", text):
        raise RuntimeError(f"Nonstatic address unmerge was not preserved; see {prepared}")


def capture(command):
    return subprocess.check_output(list(map(str, command)), text=True, timeout=120).strip()


def arguments(text):
    if os.name != "nt":
        return shlex.split(text)
    # llvm-config uses Windows command-line quoting, not POSIX shell quoting.
    import ctypes
    from ctypes import wintypes
    split = ctypes.windll.shell32.CommandLineToArgvW
    split.argtypes = [wintypes.LPCWSTR, ctypes.POINTER(ctypes.c_int)]
    split.restype = ctypes.POINTER(wintypes.LPWSTR)
    count = ctypes.c_int()
    argv = split("args " + text, ctypes.byref(count))
    if not argv:
        raise RuntimeError("Cannot parse Windows compiler flags")
    try:
        return list(argv[1:count.value])
    finally:
        ctypes.windll.kernel32.LocalFree(ctypes.cast(argv, ctypes.c_void_p))


def sdk_tool(prefix, name):
    return prefix / "bin" / (name + (".exe" if os.name == "nt" else ""))


def config(prefix, *options):
    return capture([sdk_tool(prefix, "llvm-config"), *options])


def base_sdk(base):
    if config(base, "--version") != VERSION:
        raise RuntimeError(f"Base SDK must be exactly LLVM {VERSION}")
    tools = ("llvm-ar", "llvm-config", "llc", "opt", "clang", "clang++",
             "llvm-as", "llvm-dis", "llvm-nm", "llvm-strip", "llvm-objdump", "llvm-readobj")
    for name in tools:
        if not sdk_tool(base, name).is_file():
            raise RuntimeError(f"Incomplete base SDK: missing {name}")
    if Path(config(base, "--prefix")).resolve() != base:
        raise RuntimeError("Base llvm-config does not resolve its own SDK")
    libdir = Path(config(base, "--libdir"))
    includedir = Path(config(base, "--includedir"))
    if libdir.resolve() != base / "lib" or includedir.resolve() != base / "include":
        raise RuntimeError("Base SDK requires conventional private include/lib directories")
    libraries = [libdir / name for name in arguments(config(
        base, "--link-static", "--libnames", "all"))]
    if not libraries or any(not path.is_file() for path in libraries):
        raise RuntimeError("Base SDK is missing static LLVM libraries")
    codegen = [path for path in libraries if path.name in ("libLLVMCodeGen.a", "LLVMCodeGen.lib")]
    if len(codegen) != 1:
        raise RuntimeError("Expected exactly one static LLVMCodeGen archive")
    headers = sorted(path for path in includedir.rglob("*") if path.is_file())
    if not headers or not (includedir / "llvm/Config/llvm-config.h").is_file():
        raise RuntimeError("Base SDK lacks installed LLVM configuration headers")
    # Hash all static link inputs and installed headers, not just version text.
    files = sorted(set(libraries + headers + [sdk_tool(base, name) for name in tools]))
    provider = [path for path in base.iterdir() if path.is_file()]
    if (base / ".brew").is_dir():
        provider += [path for path in (base / ".brew").rglob("*") if path.is_file()]
    hashes = {str(path.relative_to(base)): digest(path) for path in files + sorted(provider)}
    return {
        "prefix": str(base), "files": hashes,
        "cxxflags": config(base, "--cxxflags"), "ldflags": config(base, "--ldflags"),
        "system_libs": config(base, "--link-static", "--system-libs"),
        "build_mode": config(base, "--build-mode"),
        "assertions": config(base, "--assertion-mode"),
        "rtti": config(base, "--has-rtti"), "host_target": config(base, "--host-target"),
    }, libraries, codegen[0]


def clone_sdk(base, prefix):
    if os.name != "nt":
        # Keep writable containers private. Link unchanged SDK contents instead
        # of duplicating root-owned packages or following cyclic build symlinks.
        for source in base.iterdir():
            if source.name in (MARKER, "logs"):
                continue
            destination = prefix / source.name
            if source.name in ("bin", "lib", "share") and source.is_dir():
                if destination.is_symlink():
                    raise RuntimeError(f"SDK container must be a real directory: {destination}")
                destination.mkdir(exist_ok=True)
                entries = source.iterdir()
            else:
                entries = (source,)
            for entry in entries:
                target = destination / entry.name if entry != source else destination
                if target.is_symlink() or target.is_file():
                    target.unlink()
                elif target.exists():
                    raise RuntimeError(f"Use a fresh SDK prefix; directory already exists: {target}")
                if entry == base / "bin/llvm-config":
                    shutil.copy2(entry, target)
                else:
                    target.symlink_to(entry.resolve(), target_is_directory=entry.is_dir())
        return

    def copy_file(source, destination):
        destination = Path(destination)
        if destination.exists() or destination.is_symlink():
            destination.unlink()
        # A private copy avoids changing read-only attributes on the stock SDK.
        shutil.copyfile(source, destination)
        return str(destination)

    for source in base.iterdir():
        if source.name in (MARKER, "logs"):
            continue
        destination = prefix / source.name
        if source.is_dir():
            shutil.copytree(source, destination, copy_function=copy_file, dirs_exist_ok=True)
        else:
            copy_file(source, destination)


def targeted(base, prefix, work, source, logs, metadata, libraries, codegen):
    clone_sdk(base, prefix)
    if (Path(config(prefix, "--prefix")).resolve() != prefix.resolve()
            or Path(config(prefix, "--libdir")).resolve() != (prefix / "lib").resolve()
            or Path(config(prefix, "--includedir")).resolve() != (prefix / "include").resolve()):
        raise RuntimeError("Copied llvm-config does not relocate to the private SDK")
    expected = [prefix / path.relative_to(base) for path in libraries]
    actual = arguments(config(prefix, "--link-static", "--libnames", "all"))
    if actual != [path.name for path in expected]:
        raise RuntimeError("Private llvm-config resolves different static libraries")
    ar = sdk_tool(base, "llvm-ar")
    members = capture([ar, "t", codegen]).splitlines()
    matches = [(index, name) for index, name in enumerate(members) if re.fullmatch(
        r"CodeGenPrepare(?:\.cpp)?\.(?:o|obj)", Path(name.replace("\\", "/")).name)]
    if len(matches) != 1:
        raise RuntimeError("Expected exactly one CodeGenPrepare object in the base archive")
    member_index, source_member = matches[0]
    member = Path(source_member.replace("\\", "/")).name
    compiler = arguments(os.environ.get("CXX", "cl" if os.name == "nt" else "c++"))
    if not compiler or shutil.which(compiler[0]) is None:
        raise RuntimeError("Missing native C++ compiler; Windows requires a VS developer shell")
    flags = arguments(metadata["cxxflags"])
    if metadata["build_mode"] != "Release" or metadata["assertions"] != "OFF":
        raise RuntimeError("Targeted preparation requires a release SDK without assertions")
    if os.name == "nt":
        stock_object = work / ("stock-" + member)
        with stock_object.open("wb") as stream:
            subprocess.run([ar, "p", codegen, source_member], stdout=stream, check=True)
        directives = capture([sdk_tool(base, "llvm-readobj"), "--coff-directives", stock_object])
        if not re.search(r"(?:DEFAULTLIB:LIBCMT(?:\.lib)?|RuntimeLibrary=MT_StaticRelease)",
                         directives, re.IGNORECASE) or re.search(
                             r"(?:DEFAULTLIB:MSVCRT|RuntimeLibrary=MD_|LIBCMTD)", directives,
                             re.IGNORECASE):
            raise RuntimeError("Windows base SDK must use release static MSVC CRT (/MT)")
        flags += ["/std:c++17", "/O2", "/DNDEBUG", "/MT", "/DLLVM_BUILD_STATIC"]
    else:
        flags += ["-O2", "-DNDEBUG", "-fPIC", "-fvisibility-inlines-hidden",
                  "-DLLVM_BUILD_STATIC"]
    objects = []
    commands = []
    for relative, name in (("lib/CodeGen/CodeGenPrepare.cpp", member),
                           ("tools/llc/llc.cpp", "llc" + Path(member).suffix),
                           ("tools/llc/NewPMDriver.cpp", "NewPMDriver" + Path(member).suffix)):
        output = work / name
        command = [*compiler, *flags]
        if os.name == "nt":
            command += ["/c", str(source / "llvm" / relative), f"/Fo{output}"]
        else:
            command += ["-c", source / "llvm" / relative, "-o", output]
        run(command, logs / "compile.log")
        commands.append(list(map(str, command)))
        objects.append(output)
    private_archive = work / codegen.name
    # copy2 would overwrite a preexisting hard link: always unlink first.
    private_archive.unlink(missing_ok=True)
    shutil.copy2(codegen, private_archive)
    private_archive.chmod(private_archive.stat().st_mode | 0o200)
    run([ar, "rs", private_archive, objects[0]], logs / "archive.log")
    replaced = capture([ar, "t", private_archive]).splitlines()
    if (len(replaced) != len(members)
            or replaced[:member_index] != members[:member_index]
            or replaced[member_index + 1:] != members[member_index + 1:]
            or Path(replaced[member_index].replace("\\", "/")).name != member):
        raise RuntimeError("Archive replacement changed members other than CodeGenPrepare")
    destination = prefix / codegen.relative_to(base)
    private_archive.replace(destination)
    system = arguments(metadata["system_libs"])
    link = [*compiler, *objects[1:]]
    ldflags = arguments(metadata["ldflags"])
    if os.name == "nt":
        link += ["/MT", f"/Fe{work / 'llc.exe'}", "/link", *ldflags, *expected, *system]
    else:
        link += [*ldflags]
        # System dependencies can live outside the SDK (for example Homebrew).
        # Derive search paths from llvm-config's absolute system library inputs.
        for directory in sorted({str(Path(flag).parent) for flag in system
                                 if Path(flag).is_absolute()}):
            link += [f"-L{directory}", f"-Wl,-rpath,{directory}"]
        if platform.system() == "Linux":
            link += ["-Wl,--export-dynamic", "-Wl,--start-group", *expected,
                     "-Wl,--end-group", *system]
        else:
            link += ["-Wl,-export_dynamic", *expected, *system]
        link += ["-o", work / "llc"]
    if os.name == "nt":
        response = work / "link.rsp"
        response.write_text("\n".join(subprocess.list2cmdline([str(arg)])
                                      for arg in link[len(compiler):]), encoding="utf-8")
        run([*compiler, f"@{response}"], logs / "link.log")
    else:
        run(link, logs / "link.log")
    built_llc = work / ("llc.exe" if os.name == "nt" else "llc")
    built_llc.replace(sdk_tool(prefix, "llc"))
    if platform.system() == "Darwin":
        dependencies = capture(["otool", "-L", sdk_tool(prefix, "llc")])
    else:
        dependencies = capture([sdk_tool(base, "llvm-readobj"),
                                "--coff-imports" if os.name == "nt" else "--needed-libs",
                                sdk_tool(prefix, "llc")])
    if re.search(r"(?:libLLVM[^\s/]*\.(?:so|dylib)|LLVM[^\s/]*\.dll)",
                 dependencies, re.IGNORECASE):
        raise RuntimeError("Patched llc unexpectedly depends on a stock shared LLVM library")
    (logs / "llc-dependencies.log").write_text(dependencies + "\n", encoding="utf-8")
    # Retain upstream and bundled dependency licensing with the modified SDK.
    licenses = prefix / "share" / "lyra-llvm-source-licenses"
    if licenses.is_symlink():
        raise RuntimeError("SDK license directory must not link to the stock installation")
    licenses.mkdir(parents=True, exist_ok=True)
    for path in source.rglob("*"):
        if path.is_file() and path.name.upper().startswith(("LICENSE", "NOTICE", "COPYING")):
            destination = licenses / path.relative_to(source)
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(path, destination)
    shutil.copy2(PATCH, licenses / PATCH.name)
    return {"compile_commands": commands, "link_command": list(map(str, link)),
        "base_archive_member": source_member, "archive_member": replaced[member_index],
        "outputs": {str(path.relative_to(prefix)): digest(path)
                    for path in (sdk_tool(prefix, "llc"), prefix / codegen.relative_to(base))},
        "boundary": "Patched llc and static LLVMCodeGen; clang/opt/shared LLVM remain stock"}


def main():
    if sys.version_info < (3, 12):
        raise RuntimeError("Python 3.12 or newer is required")
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--prefix", required=True, type=Path)
    parser.add_argument("--base-prefix", type=Path,
                        help="Exact stock 23.1.6 SDK; patch static CodeGen and relink llc only")
    parser.add_argument("--source-archive", type=Path,
                        help="Reuse a local source archive (size and SHA-256 are verified)")
    parser.add_argument("--work-dir", type=Path)
    parser.add_argument("--jobs", type=int, default=min(5, os.cpu_count() or 1))
    args = parser.parse_args()
    if args.jobs < 1:
        parser.error("--jobs must be positive")
    # Keep the final path component unresolved so claim() can reject symlinks.
    prefix = Path(os.path.abspath(args.prefix.expanduser()))
    work = Path(os.path.abspath((args.work_dir or prefix.with_name(
        prefix.name + "-build")).expanduser()))
    if (prefix.resolve() == work.resolve() or prefix.resolve() in work.resolve().parents
            or work.resolve() in prefix.resolve().parents):
        parser.error("prefix and work directory must be separate, non-nested directories")
    base = args.base_prefix.expanduser().resolve() if args.base_prefix else None
    if base and any(path.resolve() == base or base in path.resolve().parents
                    or path.resolve() in base.parents for path in (prefix, work)):
        parser.error("base SDK, private prefix and work directory must be separate")
    metadata, libraries, codegen = base_sdk(base) if base else (None, None, None)
    compiler = arguments(os.environ.get("CXX", "cl" if os.name == "nt" else "c++"))
    compiler_version = subprocess.run(
        [*compiler, *([] if os.name == "nt" else ["--version"])],
        stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, check=False).stdout
    identity = {"schema": 2, "mode": "targeted" if base else "source",
                "version": VERSION, "source_url": URL,
                "source_sha256": SHA256, "source_size": SIZE,
                "patch_sha256": digest(PATCH), "cmake_options": OPTIONS if not base else None,
                "base_sdk": metadata, "compiler": compiler, "compiler_version": compiler_version,
                "platform": platform.system(), "architecture": platform.machine(),
                "compiler_environment": {key: os.environ.get(key) for key in
                    ("CC", "CXX", "CFLAGS", "CXXFLAGS", "LDFLAGS", "SDKROOT",
                     "MACOSX_DEPLOYMENT_TARGET", "INCLUDE", "LIB", "CL", "_CL_")}}
    state = claim(prefix, identity)
    logs = prefix / "logs"
    logs.mkdir(exist_ok=True)
    if state["complete"]:
        for name, checksum in state.get("outputs", {}).items():
            if digest(prefix / name) != checksum:
                raise RuntimeError(f"Cached patched SDK output changed: {name}")
        regress(prefix, logs)
        print(f"Verified cached LLVM SDK: {prefix}", flush=True)
        return
    for executable in (() if base else ("cmake", "ninja")):
        if shutil.which(executable) is None:
            raise RuntimeError(f"Missing prerequisite: {executable}")
    if not any(shutil.which(name) for name in ("clang++", "g++", "cl")):
        raise RuntimeError("Missing C++ compiler; on Windows use a Visual Studio developer shell")
    claim(work, identity)
    archive = args.source_archive.expanduser().resolve() if args.source_archive else work / ARCHIVE
    if args.source_archive and not archive.is_file():
        raise RuntimeError(f"Source archive does not exist: {archive}")
    if not archive.exists():
        temporary = archive.with_suffix(".download")
        print(f"Downloading {URL}", flush=True)
        with urllib.request.urlopen(URL, timeout=120) as response, temporary.open("wb") as out:
            shutil.copyfileobj(response, out)
        if temporary.stat().st_size != SIZE or digest(temporary) != SHA256:
            raise RuntimeError(f"Source checksum mismatch: {temporary}")
        temporary.replace(archive)
    if archive.stat().st_size != SIZE or digest(archive) != SHA256:
        raise RuntimeError(f"Source checksum mismatch: {archive}")
    source = work / f"llvm-project-{VERSION}.src"
    extracted = work / "source-ready.json"
    if not extracted.exists():
        if source.exists():
            shutil.rmtree(source)
        print("Extracting verified LLVM source", flush=True)
        with tarfile.open(archive, "r:xz") as tar:
            members = tar.getmembers()
            for member in members:
                if not Path(member.name).parts or Path(member.name).parts[0] != source.name:
                    raise RuntimeError(f"Unexpected source archive entry: {member.name}")
            if base:
                required = {f"{source.name}/llvm/{name}" for name in (
                    "lib/CodeGen/CodeGenPrepare.cpp", "tools/llc/llc.cpp",
                    "tools/llc/NewPMDriver.cpp", "tools/llc/NewPMDriver.h")}
                members = [member for member in members if member.isfile() and (
                    member.name in required or Path(member.name).name.upper().startswith(
                        ("LICENSE", "NOTICE", "COPYING")))]
                if not required.issubset(member.name for member in members):
                    raise RuntimeError("Pinned source archive lacks targeted compiler sources")
            tar.extractall(work, members=members, filter="data")
        # This intentionally supports only this single exact-context patch.
        lines = PATCH.read_text(encoding="utf-8").splitlines(keepends=True)[3:]
        before = "".join(line[1:] for line in lines if line.startswith((" ", "-")))
        after = "".join(line[1:] for line in lines if line.startswith((" ", "+")))
        target = source / "llvm/lib/CodeGen/CodeGenPrepare.cpp"
        original = target.read_text(encoding="utf-8")
        if original.count(before) != 1:
            raise RuntimeError("Pinned CodeGenPrepare patch context did not match exactly once")
        target.write_text(original.replace(before, after), encoding="utf-8")
        save(extracted, identity)
    elif json.loads(extracted.read_text(encoding="utf-8")) != identity:
        raise RuntimeError("Extracted source identity mismatch")
    if base:
        state.update(targeted(base, prefix, work, source, logs, metadata, libraries, codegen))
    else:
        build = work / "build"
        run(["cmake", "-S", source / "llvm", "-B", build, "-G", "Ninja",
             f"-DCMAKE_INSTALL_PREFIX={prefix}", *OPTIONS], logs / "configure.log")
        run(["cmake", "--build", build, "--parallel", args.jobs], logs / "build.log")
        run(["cmake", "--install", build], logs / "install.log")
        shutil.copy2(build / "CMakeCache.txt", logs / "CMakeCache.txt")
    regress(prefix, logs)
    state["complete"] = True
    state["regressions"] = ["static_alloca", "nonstatic_control"]
    save(prefix / MARKER, state)
    print(f"Installed and verified LLVM SDK: {prefix}", flush=True)


if __name__ == "__main__":
    try:
        main()
    except (RuntimeError, OSError, ValueError, tarfile.TarError, subprocess.SubprocessError) as error:
        print(f"error: {error}", file=sys.stderr)
        sys.exit(1)
