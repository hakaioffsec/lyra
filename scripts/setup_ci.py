#!/usr/bin/env python3
"""Install and verify the native LLVM 22 / Rust toolchain on GitHub runners."""

import hashlib
import os
from pathlib import Path
import platform
import shlex
import shutil
import subprocess
import sys
import urllib.request


RUST_VERSION = "1.98.1"
LLVM_MAJOR = "22"
WINDOWS_LLVM_URL = (
    "https://github.com/llvm/llvm-project/releases/download/llvmorg-22.1.4/"
    "clang%2Bllvm-22.1.4-x86_64-pc-windows-msvc.tar.xz"
)
WINDOWS_LLVM_SHA256 = "ed775bdaea7087c6c1aeac9498352cfcd8610d92dc4fe9eda9aecb15ce712a2c"
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
        "https://apt.llvm.org/noble/ llvm-toolchain-noble-22 main\n",
        encoding="utf-8",
    )
    run("sudo", "install", "-m", "644", str(source), "/etc/apt/sources.list.d/apt-llvm.list")
    run("sudo", "apt-get", "update")
    run(
        "sudo", "apt-get", "install", "-y", "--no-install-recommends",
        "build-essential", "pkg-config", "clang-22", "lld-22", "llvm-22",
        "llvm-22-dev", "llvm-22-tools", "libpolly-22-dev", "libclang-rt-22-dev",
        "libffi-dev", "libzstd-dev", "zlib1g-dev", "libxml2-dev", "libedit-dev", "libncurses-dev",
    )
    return Path("/usr/lib/llvm-22")


def install_macos() -> Path:
    run("xcrun", "--show-sdk-path")
    run("brew", "install", "llvm@22", "lld@22", "libffi", "zstd", "zlib", "libxml2", "pkgconf")
    prefix = Path(run("brew", "--prefix", "llvm@22", capture=True))
    libraries = [prefix / "lib"]
    for dependency in ("libffi", "zstd", "zlib", "libxml2"):
        libraries.append(Path(run("brew", "--prefix", dependency, capture=True)) / "lib")
    # llvm-sys forwards -l system dependencies, but Homebrew's keg-only libraries
    # are not in the native linker's default search path.
    export("RUSTFLAGS", " ".join(f"-Lnative={path}" for path in libraries))
    export("LIBRARY_PATH", os.pathsep.join(str(path) for path in libraries))
    add_path(Path(run("brew", "--prefix", "lld@22", capture=True)) / "bin")
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
    archive = temp / "llvm-22.1.4.tar.xz"
    download(WINDOWS_LLVM_URL, archive, WINDOWS_LLVM_SHA256)
    prefix = temp / "llvm-22"
    prefix.mkdir()
    run("tar.exe", "-xf", str(archive), "-C", str(prefix), "--strip-components=1")
    archive.unlink()
    llvm_config = prefix / "bin/llvm-config.exe"
    system_libs = shlex.split(run(str(llvm_config), "--link-static", "--system-libs", capture=True), posix=False)
    print(f"LLVM system libraries: {system_libs}", flush=True)
    # The official archive declares xml2s even when its optional XML support is
    # unused and the archive omits that library. Supply only an empty archive;
    # any real XML symbol reference still fails at link time, never a fake API.
    declared = {Path(item.strip('"')).name.lower() for item in system_libs}
    xml_library = prefix / "lib/xml2s.lib"
    if "xml2s.lib" in declared and not xml_library.exists():
        source = temp / "empty-xml2.c"
        source.write_text("/* No XML implementations: unresolved XML calls must fail. */\n", encoding="utf-8")
        obj = temp / "empty-xml2.obj"
        run(str(prefix / "bin/clang-cl.exe"), "/nologo", "/c", str(source), f"/Fo{obj}")
        run("lib.exe", "/nologo", f"/OUT:{xml_library}", str(obj))
    search_paths = [prefix / "lib", *(Path(path) for path in os.environ.get("LIB", "").split(os.pathsep) if path)]
    for library in system_libs:
        library = library.strip('"')
        if not library.lower().endswith(".lib"):
            raise RuntimeError(f"Unexpected LLVM system library flag: {library}")
        if not any((path / library).is_file() for path in search_paths):
            raise RuntimeError(f"LLVM system dependency missing from archive and MSVC SDK: {library}")
    export("LIB", os.pathsep.join(str(path) for path in search_paths))
    return prefix


def main() -> None:
    host = (platform.system(), platform.machine().lower())
    target = TARGETS.get(host)
    if target is None or target != os.environ.get("LYRA_CI_EXPECTED_TARGET"):
        raise RuntimeError(f"Unexpected native platform {host}; expected {os.environ.get('LYRA_CI_EXPECTED_TARGET')}")
    temp = Path(os.environ["RUNNER_TEMP"]) / "lyra-toolchain"
    temp.mkdir(parents=True, exist_ok=True)
    if host[0] == "Linux":
        prefix = install_linux(temp)
    elif host[0] == "Darwin":
        prefix = install_macos()
    else:
        prefix = install_windows(temp)
    suffix = ".exe" if host[0] == "Windows" else ""
    for tool in ("llvm-config", "clang", "clang++", "opt", "llc", "llvm-as", "llvm-dis", "llvm-ar"):
        if not (prefix / "bin" / (tool + suffix)).is_file():
            raise RuntimeError(f"LLVM development tool missing: {tool}")
    if not (prefix / "include/llvm-c/Core.h").is_file():
        raise RuntimeError("LLVM development headers missing")
    version = run(str(prefix / "bin" / ("llvm-config" + suffix)), "--version", capture=True)
    if version.split(".")[0] != LLVM_MAJOR:
        raise RuntimeError(f"Expected LLVM {LLVM_MAJOR}, got {version}")
    add_path(prefix / "bin")
    export("LLVM_SYS_221_PREFIX", str(prefix))
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
