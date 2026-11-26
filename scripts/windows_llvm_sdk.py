"""Restore the pinned Windows SDK's static dependencies and native llvm-config.

Only dependency libraries and llvm-config are compiled. LLVM's configure step
supplies its component graph; installed LLVM libraries and headers stay stock.
"""

import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tarfile
import urllib.request


# Versions follow the pinned LLVM source's llvm/utils/release/build_llvm_release.bat.
# Options select the required static libraries and matching MSVC runtime.
DEPENDENCIES = (
    {
        "name": "zlib", "version": "1.3.2", "root": "zlib-1.3.2",
        "url": "https://github.com/madler/zlib/releases/download/v1.3.2/zlib-1.3.2.tar.gz",
        "sha256": "bb329a0a2cd0274d05519d61c667c062e06990d72e125ee2dfa8de64f0119d16",
        "library": "zs.lib", "cmake": ".",
        "options": ["ZLIB_BUILD_TESTING=OFF", "ZLIB_BUILD_SHARED=OFF",
                    "ZLIB_BUILD_STATIC=ON", "ZLIB_INSTALL=ON"],
    },
    {
        "name": "zstd", "version": "1.5.7", "root": "zstd-1.5.7",
        "url": "https://github.com/facebook/zstd/releases/download/v1.5.7/zstd-1.5.7.tar.gz",
        "sha256": "eb33e51f49a15e023950cd7825ca74a4a2b43db8354825ac24fc1b7ee09e6fa3",
        "library": "zstd_static.lib", "cmake": "build/cmake",
        "options": ["ZSTD_BUILD_PROGRAMS=OFF", "ZSTD_BUILD_TESTS=OFF",
                    "ZSTD_BUILD_STATIC=ON", "ZSTD_BUILD_SHARED=OFF",
                    "ZSTD_USE_STATIC_RUNTIME=ON"],
    },
    {
        "name": "libxml2", "version": "2.9.12", "root": "libxml2-v2.9.12",
        "url": "https://gitlab.gnome.org/GNOME/libxml2/-/archive/v2.9.12/libxml2-v2.9.12.tar.gz",
        "sha256": "98bfa7a9a5e2a75638422050740448ee9f02bf4dc2075c9822d7747d5ff9e617",
        "library": "libxml2s.lib", "cmake": ".",
        "options": ["BUILD_SHARED_LIBS=OFF"] + [
            "LIBXML2_WITH_" + feature + "=OFF" for feature in (
                "C14N", "CATALOG", "DEBUG", "DOCB", "FTP", "HTML", "HTTP",
                "ICONV", "ICU", "ISO8859X", "LEGACY", "LZMA", "MEM_DEBUG",
                "MODULES", "PATTERN", "PROGRAMS", "PUSH", "PYTHON", "READER",
                "REGEXPS", "RUN_DEBUG", "SCHEMAS", "SCHEMATRON", "TESTS",
                "THREAD_ALLOC", "VALID", "WRITER", "XINCLUDE", "XPATH", "XPTR",
                "ZLIB")
        ] + ["LIBXML2_WITH_" + feature + "=ON"
             for feature in ("OUTPUT", "SAX1", "THREADS", "TREE")],
    },
)


def _digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def _source(dependency, directory):
    archive = directory / (dependency["root"] + ".tar.gz")
    if not archive.exists():
        temporary = archive.with_suffix(".download")
        with urllib.request.urlopen(dependency["url"], timeout=120) as response:
            with temporary.open("wb") as output:
                shutil.copyfileobj(response, output)
        if _digest(temporary) != dependency["sha256"]:
            raise RuntimeError(f"Dependency checksum mismatch: {temporary}")
        temporary.replace(archive)
    if _digest(archive) != dependency["sha256"]:
        raise RuntimeError(f"Dependency checksum mismatch: {archive}")
    source = directory / dependency["root"]
    if source.is_symlink():
        raise RuntimeError(f"Refusing linked dependency source: {source}")
    if source.exists():
        shutil.rmtree(source)
    with tarfile.open(archive, "r:gz") as tar:
        members = tar.getmembers()
        if any(not Path(member.name).parts or
               Path(member.name).parts[0] != dependency["root"] for member in members):
            raise RuntimeError(f"Unexpected dependency archive layout: {archive}")
        tar.extractall(directory, members=members, filter="data")
    return source


def _cmake_value(text, name):
    values = re.findall(r"^set\(" + re.escape(name) + r"\s+([^\r\n]*?)\)\s*$",
                        text, re.MULTILINE)
    if len(values) != 1 or any(token in values[0] for token in ("${", "$<", ";;")):
        raise RuntimeError(f"Missing or nonliteral stock CMake setting: {name}")
    return values[0].strip().strip('"')


def is_release_static_crt(directives):
    text = directives.replace('"', "")
    static = re.search(
        r"(?:DEFAULTLIB:LIBCMT(?:\.lib)?|RuntimeLibrary=MT_StaticRelease)(?=\s|$)",
        text, re.IGNORECASE)
    incompatible = re.search(
        r"DEFAULTLIB:(?:MSVCRTD?|LIBCMTD)(?:\.lib)?(?=\s|$)|RuntimeLibrary=(?:MD_|MTd_)",
        text, re.IGNORECASE)
    return static is not None and incompatible is None


def _static_crt(base, path, capture, logs):
    directives = capture([base / "bin/llvm-readobj.exe", "--coff-directives", path])
    (logs / (path.name + "-directives.log")).write_text(directives + "\n", encoding="utf-8")
    if not is_release_static_crt(directives):
        raise RuntimeError(f"Expected release static MSVC CRT (/MT): {path}")


def _system_libraries(build_variables, dependencies, installs):
    """Correct upstream's Windows-only filename loss in generated metadata.

    GetLibraryName strips lib from libxml2s.lib; static zstd keeps an absolute
    build path. Use the real installed filenames, never duplicate/alias archives.
    All other upstream-generated metadata and system dependencies are preserved.
    """
    text = build_variables.read_text(encoding="utf-8")
    pattern = r'^#define LLVM_SYSTEM_LIBS "([^"\n]*)"$'
    matches = list(re.finditer(pattern, text, re.MULTILINE))
    if len(matches) != 1:
        raise RuntimeError("Expected one generated LLVM_SYSTEM_LIBS definition")
    original = matches[0].group(1)
    normalized = original
    # Upstream does not quote absolute system libraries in this C string.
    # Replace complete known paths before splitting, including paths with spaces.
    for dependency in dependencies:
        library = installs[dependency["name"]] / "lib" / dependency["library"]
        normalized = normalized.replace(library.as_posix(), dependency["library"])
        normalized = normalized.replace(str(library).replace("\\", "\\\\"),
                                        dependency["library"])
    aliases = {"zs.lib": "zs.lib", "zstd_static.lib": "zstd_static.lib",
               "xml2s.lib": "libxml2s.lib", "libxml2s.lib": "libxml2s.lib"}
    libraries = []
    found = set()
    for library in normalized.split():
        basename = library.replace("\\", "/").rsplit("/", 1)[-1]
        if basename in aliases:
            library = aliases[basename]
            found.add(library)
        if not re.fullmatch(r"[A-Za-z0-9_+.-]+\.lib", library):
            raise RuntimeError(f"Unexpected generated Windows system library: {library}")
        libraries.append(library)
    if found != {dependency["library"] for dependency in dependencies}:
        raise RuntimeError("Generated metadata did not enable all pinned dependencies")
    replacement = '#define LLVM_SYSTEM_LIBS "' + " ".join(libraries) + '"'
    match = matches[0]
    build_variables.write_text(text[:match.start()] + replacement + text[match.end():],
                               encoding="utf-8")
    return original, libraries


def prepare(base, prefix, work, source, metadata, run, capture):
    """Repair a cloned Windows SDK; return output hashes and build provenance.

    The caller owns/claims prefix and work, retains base metadata in its cache
    identity, and must requery the private llvm-config before linking consumers.
    """
    if os.name != "nt":
        raise RuntimeError("Windows SDK preparation requires a native Windows host")
    for executable in ("cmake", "ninja", "cl", "link"):
        if shutil.which(executable) is None:
            raise RuntimeError(f"Missing Windows SDK prerequisite: {executable}")
    stock_config = base / "bin/llvm-config.exe"
    private_config = prefix / "bin/llvm-config.exe"
    stock_cmake_path = base / "lib/cmake/llvm/LLVMConfig.cmake"
    stock_cmake = stock_cmake_path.read_text(encoding="utf-8")
    if (metadata["build_mode"] != "Release" or metadata["assertions"] != "OFF"
            or metadata["rtti"] != "NO" or _cmake_value(stock_cmake, "LLVM_ENABLE_EH") != "OFF"):
        raise RuntimeError("Windows repair requires the pinned release SDK's EH/RTTI/assertion ABI")
    comparisons = [("--version",), ("--build-mode",), ("--assertion-mode",),
                   ("--has-rtti",), ("--host-target",), ("--targets-built",),
                   ("--components",), ("--link-static", "--libnames", "all")]
    stock_outputs = {" ".join(options): capture([stock_config, *options])
                     for options in comparisons}
    targets = stock_outputs["--targets-built"].split()
    if targets != _cmake_value(stock_cmake, "LLVM_TARGETS_TO_BUILD").split(";"):
        raise RuntimeError("Stock llvm-config and CMake target lists disagree")

    directory = work / "windows-dependencies"
    directory.mkdir(exist_ok=True)
    logs = prefix / "logs"
    licenses = prefix / "share/lyra-llvm-windows-dependencies"
    if licenses.is_symlink():
        raise RuntimeError("Dependency license directory must be private")
    licenses.mkdir(parents=True, exist_ok=True)
    outputs = {}
    commands = []
    records = []
    installs = {}

    def execute(command, log):
        commands.append(list(map(str, command)))
        run(command, logs / log)

    def install_file(origin, destination):
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.unlink(missing_ok=True)
        shutil.copyfile(origin, destination)
        outputs[str(destination.relative_to(prefix))] = _digest(destination)

    common = ["-G", "Ninja", "-DCMAKE_BUILD_TYPE=Release",
              "-DCMAKE_C_COMPILER=cl", "-DCMAKE_CXX_COMPILER=cl",
              "-DCMAKE_MSVC_RUNTIME_LIBRARY=MultiThreaded"]
    for dependency in DEPENDENCIES:
        dependency_source = _source(dependency, directory)
        build = directory / (dependency["name"] + "-build")
        install = directory / (dependency["name"] + "-install")
        installs[dependency["name"]] = install
        execute(["cmake", "-S", dependency_source / dependency["cmake"], "-B", build,
                 *common, f"-DCMAKE_INSTALL_PREFIX={install.as_posix()}",
                 *("-D" + option for option in dependency["options"])],
                dependency["name"] + "-configure.log")
        execute(["cmake", "--build", build, "--target", "install", "--parallel",
                 str(min(5, os.cpu_count() or 1))], dependency["name"] + "-build.log")
        library = install / "lib" / dependency["library"]
        if not library.is_file() or not capture([base / "bin/llvm-ar.exe", "t", library]):
            raise RuntimeError(f"Dependency build did not produce a real archive: {library}")
        _static_crt(base, library, capture, logs)
        install_file(library, prefix / "lib" / library.name)
        notices = [path for path in dependency_source.rglob("*") if path.is_file()
                   and path.name.upper().startswith(("LICENSE", "NOTICE", "COPYING", "COPYRIGHT"))]
        if not notices:
            raise RuntimeError(f"Dependency source lacks license notices: {dependency['name']}")
        for notice in notices:
            install_file(notice, licenses / dependency["name"] / notice.relative_to(dependency_source))
        records.append({key: dependency[key] for key in
                        ("name", "version", "url", "sha256", "library", "options")})

    # Configure only: no LLVM build target is ever invoked, including Support.
    build = work / "windows-llvm-config"
    options = [
        "-DLLVM_ENABLE_PROJECTS=", "-DLLVM_ENABLE_RUNTIMES=",
        "-DLLVM_TARGETS_TO_BUILD=" + ";".join(targets),
        "-DLLVM_DEFAULT_TARGET_TRIPLE=" + metadata["host_target"],
        "-DLLVM_HOST_TRIPLE=" + _cmake_value(stock_cmake, "LLVM_HOST_TRIPLE"),
        "-DLLVM_ENABLE_ASSERTIONS=OFF", "-DLLVM_ENABLE_EH=OFF", "-DLLVM_ENABLE_RTTI=OFF",
        "-DLLVM_INCLUDE_TESTS=OFF", "-DLLVM_INCLUDE_BENCHMARKS=OFF",
        "-DLLVM_INCLUDE_EXAMPLES=OFF", "-DLLVM_BUILD_LLVM_DYLIB=OFF",
        "-DLLVM_LINK_LLVM_DYLIB=OFF", "-DBUILD_SHARED_LIBS=OFF", "-DLLVM_USE_CRT_RELEASE=MT",
        "-DLLVM_ENABLE_ZLIB=FORCE_ON", "-DLLVM_ENABLE_ZSTD=FORCE_ON",
        "-DLLVM_ENABLE_LIBXML2=FORCE_ON", "-DLLVM_USE_STATIC_ZSTD=ON",
        "-DCMAKE_C_FLAGS=-DLIBXML_STATIC", "-DCMAKE_CXX_FLAGS=-DLIBXML_STATIC",
        f"-DZLIB_INCLUDE_DIR={(installs['zlib'] / 'include').as_posix()}",
        f"-DZLIB_LIBRARY={(installs['zlib'] / 'lib/zs.lib').as_posix()}",
        f"-DZLIB_LIBRARY_RELEASE={(installs['zlib'] / 'lib/zs.lib').as_posix()}",
        f"-Dzstd_INCLUDE_DIR={(installs['zstd'] / 'include').as_posix()}",
        f"-Dzstd_LIBRARY={(installs['zstd'] / 'lib/zstd_static.lib').as_posix()}",
        f"-Dzstd_STATIC_LIBRARY={(installs['zstd'] / 'lib/zstd_static.lib').as_posix()}",
        f"-DLIBXML2_INCLUDE_DIR={(installs['libxml2'] / 'include/libxml2').as_posix()}",
        f"-DLIBXML2_LIBRARIES={(installs['libxml2'] / 'lib/libxml2s.lib').as_posix()}",
    ]
    for setting in ("LLVM_ABI_BREAKING_CHECKS", "LLVM_ENABLE_THREADS", "LLVM_ENABLE_FFI",
                    "LLVM_ENABLE_LIBEDIT", "LLVM_ENABLE_CURL", "LLVM_ENABLE_HTTPLIB",
                    "LLVM_ENABLE_DIA_SDK", "LLVM_ENABLE_TELEMETRY", "LLVM_ENABLE_PLUGINS",
                    "LLVM_ENABLE_EXPENSIVE_CHECKS", "LLVM_ENABLE_UNWIND_TABLES"):
        options.append("-D" + setting + "=" + _cmake_value(stock_cmake, setting))
    execute(["cmake", "-S", source / "llvm", "-B", build, *common,
             f"-DCMAKE_INSTALL_PREFIX={prefix.as_posix()}", *options], "llvm-config-configure.log")
    generated = build / "tools/llvm-config"
    fragments = [generated / name for name in
                 ("BuildVariables.inc", "LibraryDependencies.inc", "ExtensionDependencies.inc")]
    if any(not path.is_file() for path in fragments):
        raise RuntimeError("LLVM configure did not generate llvm-config metadata")
    before = {path.name: _digest(path) for path in fragments}
    original_system, system = _system_libraries(fragments[0], DEPENDENCIES, installs)
    stock_system = [Path(library).name for library in metadata["system_libs"].split()]
    stock_system = ["libxml2s.lib" if name == "xml2s.lib" else name for name in stock_system]
    if system != stock_system:
        raise RuntimeError("Generated metadata changed the stock system dependency set")

    # Stock flags keep the ABI and installed headers authoritative. Only the
    # uninstalled config.h and tool metadata come from the configure-only tree.
    obj = build / "llvm-config.obj"
    compile_args = ["/std:c++17", "/O2", "/DNDEBUG", "/MT", "/DLLVM_BUILD_STATIC",
                    f"/I{build / 'include'}", f"/I{generated}", "/c",
                    str(source / "llvm/tools/llvm-config/llvm-config.cpp"), f"/Fo{obj}"]
    response = build / "compile.rsp"
    response.write_text(metadata["cxxflags"] + " " + subprocess.list2cmdline(compile_args) + "\n",
                        encoding="utf-8")
    execute(["cl", f"@{response}"], "llvm-config-compile.log")
    _static_crt(base, obj, capture, logs)
    link_libraries = capture([stock_config, "--link-static", "--libnames", "support",
                              "targetparser"]).split()
    if any(not re.fullmatch(r"[A-Za-z0-9_+.-]+\.lib", name) for name in link_libraries):
        raise RuntimeError("Unexpected stock llvm-config link libraries")
    executable = build / "llvm-config.exe"
    link = ["/NOLOGO", f"/OUT:{executable}", str(obj), f"/LIBPATH:{prefix / 'lib'}",
            *(str(base / "lib" / name) for name in link_libraries), *system]
    response = build / "link.rsp"
    response.write_text(subprocess.list2cmdline(link) + "\n", encoding="utf-8")
    execute(["link", f"@{response}"], "llvm-config-link.log")
    install_file(executable, private_config)

    for args in comparisons:
        if capture([private_config, *args]) != stock_outputs[" ".join(args)]:
            raise RuntimeError("Rebuilt llvm-config changed stock " + " ".join(args))
    actual_system = capture([private_config, "--link-static", "--system-libs"]).split()
    if actual_system != system:
        raise RuntimeError("Rebuilt llvm-config system dependencies differ from generated metadata")
    if any(not (prefix / "lib" / dependency["library"]).is_file() for dependency in DEPENDENCIES):
        raise RuntimeError("Rebuilt llvm-config references an absent static dependency")
    if Path(capture([private_config, "--prefix"])).resolve() != prefix.resolve():
        raise RuntimeError("Rebuilt llvm-config did not relocate to the private SDK")
    imports = capture([base / "bin/llvm-readobj.exe", "--coff-imports", private_config])
    (logs / "llvm-config-imports.log").write_text(imports + "\n", encoding="utf-8")
    if re.search(r"(?:LLVM[^\s]*\.dll|(?:msvcrt|msvcp|vcruntime|ucrtbase)\w*\.dll|api-ms-win-crt-)",
                 imports, re.IGNORECASE):
        raise RuntimeError("Rebuilt llvm-config imports a shared LLVM or CRT library")

    provenance = {
        "dependencies": records, "commands": commands,
        "stock_cmake_sha256": _digest(stock_cmake_path),
        "source_sha256": _digest(source / "llvm/tools/llvm-config/llvm-config.cpp"),
        "generated_metadata_before": before,
        "generated_metadata_after": {path.name: _digest(path) for path in fragments},
        "original_generated_system_libs": original_system, "system_libs": system,
        "stock_outputs": stock_outputs, "compile_arguments": compile_args,
        "stock_cxxflags": metadata["cxxflags"], "link_arguments": link,
        "boundary": "Dependencies and llvm-config rebuilt; LLVM libraries and headers remain stock",
    }
    manifest = licenses / "provenance.json"
    manifest.write_text(json.dumps(provenance, indent=2) + "\n", encoding="utf-8")
    outputs[str(manifest.relative_to(prefix))] = _digest(manifest)
    return {"outputs": outputs, "windows_sdk": provenance}
