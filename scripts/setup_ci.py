#!/usr/bin/env python3
"""Install the pinned native Rust toolchain and prepare the private LLVM SDK."""

import hashlib
import json
import os
from pathlib import Path
import platform
import shlex
import shutil
import subprocess
import sys
import tarfile
import tomllib
import urllib.request

from build_llvm import (
    ARCHIVE as LLVM_SOURCE_ARCHIVE,
    MARKER as LLVM_SDK_MARKER,
    SHA256 as LLVM_SOURCE_SHA256,
    URL as LLVM_SOURCE_URL,
    VERSION as LLVM_VERSION,
)


ROOT = Path(__file__).resolve().parent.parent
RUST_VERSION = tomllib.loads((ROOT / "rust-toolchain.toml").read_text(encoding="utf-8"))["toolchain"]["channel"]
LLVM_MAJOR = LLVM_VERSION.split(".")[0]
WINDOWS_LLVM_URL = (
    f"https://github.com/llvm/llvm-project/releases/download/llvmorg-{LLVM_VERSION}/"
    f"clang%2Bllvm-{LLVM_VERSION}-x86_64-pc-windows-msvc.tar.xz"
)
WINDOWS_LLVM_SHA256 = "16af05919cf36958dfb4ddaa4fc4d9d4ff577f5daeb76bc112ea49cd8ced82b2"
APT_KEY_FINGERPRINT = "6084F3CF814B57C1CF12EFD515CF4D18AF4F7421"
TARGETS = {
    ("Linux", "x86_64"): "x86_64-unknown-linux-gnu",
    ("Darwin", "arm64"): "aarch64-apple-darwin",
    ("Windows", "amd64"): "x86_64-pc-windows-msvc",
}


def run(*args: str, capture: bool = False) -> str:
    print("+ " + shlex.join(str(arg) for arg in args), flush=True)
    executable = shutil.which(str(args[0]))
    if executable is None:
        raise RuntimeError(f"Required executable not found: {args[0]}")
    result = subprocess.run(
        [executable, *(str(arg) for arg in args[1:])],
        check=True,
        text=True,
        stdout=subprocess.PIPE if capture else None,
        timeout=2400,
    )
    return result.stdout.strip() if capture else ""


def export(name: str, value: str) -> None:
    if "\n" in value or "\r" in value:
        raise RuntimeError(f"Unexpected multiline environment value: {name}")
    os.environ[name] = value
    with Path(os.environ["GITHUB_ENV"]).open("a", encoding="utf-8") as stream:
        stream.write(f"{name}={value}\n")


def add_path(path: Path) -> None:
    os.environ["PATH"] = str(path) + os.pathsep + os.environ["PATH"]
    with Path(os.environ["GITHUB_PATH"]).open("a", encoding="utf-8") as stream:
        stream.write(f"{path}\n")


def download(url: str, destination: Path, digest: str | None = None) -> None:
    print(f"Downloading {url}", flush=True)
    sha256 = hashlib.sha256()
    with urllib.request.urlopen(url, timeout=120) as source, destination.open("wb") as output:
        while block := source.read(1024 * 1024):
            output.write(block)
            sha256.update(block)
    if digest is not None and sha256.hexdigest() != digest:
        raise RuntimeError(f"SHA256 mismatch for {destination.name}: {sha256.hexdigest()}")


def install_linux(temp: Path) -> Path:
    release = platform.freedesktop_os_release()
    if release.get("ID") != "ubuntu" or release.get("VERSION_CODENAME") != "noble":
        raise RuntimeError(f"Expected Ubuntu noble, got {release}")
    run("sudo", "apt-get", "update")
    run("sudo", "apt-get", "install", "-y", "--no-install-recommends", "ca-certificates", "gnupg")
    key = temp / "apt-llvm.asc"
    download("https://apt.llvm.org/llvm-snapshot.gpg.key", key)
    key_info = run("gpg", "--batch", "--show-keys", "--with-colons", str(key), capture=True)
    fingerprints = [line.split(":")[9] for line in key_info.splitlines() if line.startswith("fpr:")]
    if not fingerprints or fingerprints[0] != APT_KEY_FINGERPRINT:
        raise RuntimeError(f"Unexpected apt.llvm.org signing key: {fingerprints}")
    run("sudo", "install", "-m", "644", str(key), "/usr/share/keyrings/apt-llvm.asc")
    source = temp / "apt-llvm.list"
    source.write_text(
        "deb [arch=amd64 signed-by=/usr/share/keyrings/apt-llvm.asc] "
        f"https://apt.llvm.org/noble/ llvm-toolchain-noble-{LLVM_MAJOR} main\n",
        encoding="utf-8",
    )
    run("sudo", "install", "-m", "644", str(source), "/etc/apt/sources.list.d/apt-llvm.list")
    run("sudo", "apt-get", "update")
    packages = [
        f"clang-{LLVM_MAJOR}", f"lld-{LLVM_MAJOR}", f"llvm-{LLVM_MAJOR}",
        f"llvm-{LLVM_MAJOR}-dev", f"llvm-{LLVM_MAJOR}-tools",
        f"libpolly-{LLVM_MAJOR}-dev", f"libclang-rt-{LLVM_MAJOR}-dev",
    ]
    pinned = []
    for package in packages:
        versions = run("apt-cache", "madison", package, capture=True)
        matches = [
            line.split("|")[1].strip() for line in versions.splitlines()
            if len(line.split("|")) >= 3
            and line.split("|")[1].strip().split(":")[-1].split("~")[0].split("-")[0] == LLVM_VERSION
        ]
        if not matches:
            raise RuntimeError(f"No exact LLVM {LLVM_VERSION} package available: {package}")
        pinned.append(f"{package}={matches[0]}")
    run(
        "sudo", "apt-get", "install", "-y", "--no-install-recommends",
        "build-essential", "pkg-config", "cmake", "ninja-build", *pinned,
        "libffi-dev", "libzstd-dev", "zlib1g-dev", "libxml2-dev", "libedit-dev", "libncurses-dev",
    )
    return Path(f"/usr/lib/llvm-{LLVM_MAJOR}")


def install_macos() -> Path:
    export("SDKROOT", run("xcrun", "--sdk", "macosx", "--show-sdk-path", capture=True))
    run("brew", "update")
    formulae = (f"llvm@{LLVM_MAJOR}", f"lld@{LLVM_MAJOR}")
    info = json.loads(run("brew", "info", "--json=v2", *formulae, capture=True))
    for formula in info["formulae"]:
        if formula["versions"]["stable"] != LLVM_VERSION:
            raise RuntimeError(f"Expected Homebrew {formula['name']} {LLVM_VERSION}, got {formula['versions']['stable']}")
    run("brew", "install", "--force-bottle", *formulae, "cmake", "ninja", "pkgconf")
    prefix = Path(run("brew", "--prefix", formulae[0], capture=True))
    libraries = []
    for dependency in run("brew", "deps", "--installed", formulae[0], capture=True).splitlines():
        library = Path(run("brew", "--prefix", dependency, capture=True)) / "lib"
        if library.is_dir():
            libraries.append(library)
    # llvm-sys needs explicit paths for keg-only system dependencies.
    export("CARGO_ENCODED_RUSTFLAGS", "\x1f".join(f"-Lnative={path}" for path in libraries))
    export("LIBRARY_PATH", os.pathsep.join(str(path) for path in libraries))
    add_path(Path(run("brew", "--prefix", formulae[1], capture=True)) / "bin")
    return prefix


def initialize_msvc(temp: Path) -> None:
    vswhere = Path(os.environ["ProgramFiles(x86)"]) / "Microsoft Visual Studio/Installer/vswhere.exe"
    installation = run(
        str(vswhere), "-latest", "-products", "*", "-requires",
        "Microsoft.VisualStudio.Component.VC.Tools.x86.x64", "-property", "installationPath", capture=True,
    )
    if not installation:
        raise RuntimeError("No Visual Studio installation with native x64 build tools")
    script = temp / "msvc-env.cmd"
    script.write_text(
        f'@call "{installation}\\Common7\\Tools\\VsDevCmd.bat" -no_logo -arch=x64 -host_arch=x64\n'
        '@if errorlevel 1 exit /b %errorlevel%\n@set\n',
        encoding="utf-8",
    )
    values = run("cmd.exe", "/d", "/c", str(script), capture=True)
    # Import the actual developer environment, rather than guessing SDK versions.
    for line in values.splitlines():
        name, separator, value = line.partition("=")
        if not separator or not name or name.startswith("="):
            continue
        if name.upper() == "PATH":
            for entry in reversed(value.split(os.pathsep)):
                if entry:
                    add_path(Path(entry))
        elif name.upper() not in {"GITHUB_ENV", "GITHUB_PATH"} and not name.upper().startswith(("GITHUB_", "RUNNER_")):
            if os.environ.get(name) != value:
                export(name, value)
    run("where.exe", "cl.exe")
    run("where.exe", "link.exe")
    run("where.exe", "lib.exe")


def install_windows(temp: Path) -> Path:
    initialize_msvc(temp)
    archive = temp / f"llvm-{LLVM_VERSION}.tar.xz"
    download(WINDOWS_LLVM_URL, archive, WINDOWS_LLVM_SHA256)
    prefix = temp / "stock"
    unpacked = temp / "llvm-unpack"
    unpacked.mkdir()
    # Avoid Git tar's drive-letter parsing and native tar's stalled extraction.
    # Python's data filter also rejects paths and links escaping the destination.
    print("Extracting verified LLVM archive with Python", flush=True)
    with tarfile.open(archive, mode="r:xz") as bundle:
        for index, member in enumerate(bundle, 1):
            bundle.extract(member, unpacked, filter="data")
            if index % 1000 == 0:
                print(f"Extracted {index} LLVM archive entries", flush=True)
    roots = list(unpacked.iterdir())
    if len(roots) != 1 or not roots[0].is_dir():
        raise RuntimeError("Expected one top-level LLVM bundle directory")
    roots[0].rename(prefix)
    unpacked.rmdir()
    print("LLVM archive extraction complete", flush=True)
    archive.unlink()
    return prefix


def configure_windows_linker(prefix: Path) -> None:
    llvm_config = prefix / "bin/llvm-config.exe"
    system_libs = shlex.split(run(str(llvm_config), "--link-static", "--system-libs", capture=True), posix=False)
    print(f"LLVM system libraries: {system_libs}", flush=True)
    search_paths = [prefix / "lib", *(Path(path) for path in os.environ.get("LIB", "").split(os.pathsep) if path)]
    for library in system_libs:
        library = library.strip('"')
        if not library.lower().endswith(".lib"):
            raise RuntimeError(f"Unexpected LLVM system library flag: {library}")
        if not any((path / library).is_file() for path in search_paths):
            raise RuntimeError(f"LLVM system dependency missing from archive and MSVC SDK: {library}")
    export("LIB", os.pathsep.join(str(path) for path in search_paths))
    # LLVM's official MSVC archive uses /MT. Rust must use that CRT too.
    # With crt-static, llvm-sys bundles system import libraries into its rlib;
    # rustc needs explicit native search paths, not only the linker's LIB.
    # Encoded flags keep SDK paths containing spaces as single arguments.
    rustflags = ["-C", "target-feature=+crt-static"]
    rustflags.extend(f"-Lnative={path}" for path in search_paths)
    export("CARGO_ENCODED_RUSTFLAGS", "\x1f".join(rustflags))


def main() -> None:
    host = (platform.system(), platform.machine().lower())
    target = TARGETS.get(host)
    if target is None or target != os.environ.get("LYRA_CI_EXPECTED_TARGET"):
        raise RuntimeError(f"Unexpected native platform {host}; expected {os.environ.get('LYRA_CI_EXPECTED_TARGET')}")
    temp = Path(os.environ["RUNNER_TEMP"]) / "lyra-toolchain"
    temp.mkdir(parents=True, exist_ok=True)
    if host[0] == "Linux":
        stock = install_linux(temp)
    elif host[0] == "Darwin":
        stock = install_macos()
    else:
        stock = install_windows(temp)
    suffix = ".exe" if host[0] == "Windows" else ""
    version = run(str(stock / "bin" / ("llvm-config" + suffix)), "--version", capture=True)
    if version != LLVM_VERSION:
        raise RuntimeError(f"Expected stock LLVM {LLVM_VERSION}, got {version}")
    add_path(stock / "bin")
    if host[0] != "Windows":
        export("CXX", shlex.quote(str(stock / "bin/clang++")))
    downloads = temp / "downloads"
    downloads.mkdir(exist_ok=True)
    source = downloads / LLVM_SOURCE_ARCHIVE
    if not source.exists():
        download(LLVM_SOURCE_URL, source, LLVM_SOURCE_SHA256)
    prefix = temp / "private"
    try:
        run(
            sys.executable, str(ROOT / "scripts/build_llvm.py"),
            "--prefix", str(prefix), "--base-prefix", str(stock),
            "--work-dir", str(temp / "build"), "--source-archive", str(source), "--jobs", "4",
        )
    finally:
        receipt = prefix / LLVM_SDK_MARKER
        if receipt.is_file():
            logs = temp.parent / "lyra-ci-logs"
            logs.mkdir(exist_ok=True)
            shutil.copyfile(receipt, logs / "sdk-receipt.json")
    if host[0] == "Windows":
        configure_windows_linker(prefix)
    for tool in ("llvm-config", "clang", "clang++", "opt", "llc", "llvm-as", "llvm-dis", "llvm-ar"):
        if not (prefix / "bin" / (tool + suffix)).is_file():
            raise RuntimeError(f"LLVM development tool missing: {tool}")
    for header in ("llvm-c/Core.h", "llvm/Transforms/Utils/CodeExtractor.h", "llvm/Config/abi-breaking.h"):
        if not (prefix / "include" / header).is_file():
            raise RuntimeError(f"LLVM development header missing: {header}")
    version = run(str(prefix / "bin" / ("llvm-config" + suffix)), "--version", capture=True)
    if version != LLVM_VERSION:
        raise RuntimeError(f"Expected private LLVM {LLVM_VERSION}, got {version}")
    add_path(prefix / "bin")
    export("LLVM_SYS_231_PREFIX", str(prefix))
    export("LYRA_LLVM_BIN", str(prefix / "bin"))
    export("RUSTUP_TOOLCHAIN", RUST_VERSION)
    run("rustup", "toolchain", "install", RUST_VERSION, "--profile", "minimal", "--component", "rustfmt")
    rust_info = run("rustc", "-vV", capture=True)
    print(rust_info, flush=True)
    details = dict(line.split(": ", 1) for line in rust_info.splitlines() if ": " in line)
    if details.get("host") != target or details.get("release") != RUST_VERSION:
        raise RuntimeError(f"Unexpected Rust compiler: {rust_info}")
    if details.get("LLVM version", "").split(".")[0] != LLVM_MAJOR:
        raise RuntimeError(f"Rust must emit LLVM {LLVM_MAJOR} IR: {rust_info}")
    run("clang", "--version")
    run("opt", "--version")
    print(f"Verified native target {target}, Rust {RUST_VERSION}, LLVM {version}", flush=True)


if __name__ == "__main__":
    try:
        main()
    except (OSError, RuntimeError, subprocess.SubprocessError) as error:
        print(f"Toolchain setup failed: {error}", file=sys.stderr, flush=True)
        sys.exit(1)
