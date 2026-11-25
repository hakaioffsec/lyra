#!/usr/bin/env python3
# Copyright (c) 2026 Hakai Offensive Security.
# SPDX-License-Identifier: GPL-3.0-or-later
"""Export functions, assembly, and Hex-Rays pseudocode using local IDA 9 idalib.

Install the supplied idalib/python package into a Python environment compatible
with IDAPython, then run this script using that environment's Python. Example:
  python scripts/export_ida.py fixture.so --ida-dir /path/to/ida \
      --output target/ida-fresh --function float_add --function float_mul

The output directory must not exist. The input is copied before analysis; only
that copy receives a disposable database. HOME and IDAUSR use a private temporary
directory, with a copy of ida.reg from --ida-user-dir to inherit accepted terms.
That private copy is deleted on exit, not exported. No activation, license
changes, MCP bridge, or user configuration edits occur. A valid locally
available license and previously accepted IDA terms are required.
Native console output is not retained because it can contain license metadata.
The worker is killed and reaped if analysis exceeds 600 seconds.
"""

import argparse
import hashlib
import importlib.metadata
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile


def write_report(directory, report):
    (directory / "analysis.json").write_text(
        json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    lines = [f"Status: {report['status']}",
             f"Input SHA256: {report['input_sha256']}",
             "Versions: " + json.dumps(report["versions"], sort_keys=True),
             "Analysis settings: " + json.dumps(report["analysis_settings"], sort_keys=True),
             "", "Functions:"]
    for function in report["functions"]:
        lines.append(f"{function['address']} {function['name']}")
    for function in report["selected"]:
        lines.extend(["", f"=== {function['name']} ({function['address']}) ===",
                      "Assembly:"])
        for instruction in function["assembly"]:
            lines.append(f"{instruction['address']}  {instruction['bytes']}  "
                         f"{instruction['text']}")
        lines.append("Recovered CFG:")
        for block in function["cfg"]:
            lines.append(f"{block['start']}..{block['end']} -> "
                         + (", ".join(block["successors"]) or "(exit)"))
        lines.extend(["Pseudocode:", function["pseudocode"] or "<unavailable>"])
    if report["errors"]:
        lines.extend(["", "Errors:", *report["errors"]])
    (directory / "analysis.txt").write_text("\n".join(lines) + "\n", encoding="utf-8")


def analyze(directory):
    # The launcher imports idapro before running this module in the worker.
    import idapro
    import ida_auto
    import ida_bytes
    import ida_funcs
    import ida_gdl
    import ida_hexrays
    import ida_ida
    import ida_kernwin
    import ida_lines
    import idaapi
    import idautils
    import idc

    report = json.loads((directory / "analysis.json").read_text(encoding="utf-8"))
    opened = False
    try:
        report["versions"].update({
            "idalib": list(idapro.get_library_version() or ()),
            "ida_kernel": ida_kernwin.get_kernel_version(),
            "idapython": list(_idapython_version),
            "ida_sdk": idaapi.IDA_SDK_VERSION,
        })
        report["status"] = "analyzing"
        write_report(directory, report)
        result = idapro.open_database(str(directory / report["database_input"]), True, "-c")
        if result != 0:
            raise RuntimeError(f"open_database returned {result}")
        opened = True
        if not ida_auto.auto_wait():
            raise RuntimeError("IDA auto-analysis was cancelled")
        report["processor"] = ida_ida.inf_get_procname()
        report["bits"] = 64 if ida_ida.inf_is_64bit() else 32 if ida_ida.inf_is_32bit_exactly() else 16
        report["analysis_settings"].update({
            "analysis_flags": ida_ida.inf_get_af(),
            "analysis_flags2": ida_ida.inf_get_af2(),
            "compiler_id": int(ida_ida.inf_get_cc_id()),
        })
        for address in idautils.Functions():
            report["functions"].append({
                "address": hex(address), "name": ida_funcs.get_func_name(address),
                "chunks": [[hex(start), hex(end)] for start, end in idautils.Chunks(address)],
            })
        decompiler = ida_hexrays.init_hexrays_plugin()
        report["versions"]["hexrays"] = ida_hexrays.get_hexrays_version() if decompiler else None
        if not decompiler:
            report["errors"].append("Hex-Rays decompiler initialization failed")
        for name in report["requested_functions"]:
            address = int(name, 16) if re.fullmatch(r"0[xX][0-9a-fA-F]+", name) else idc.get_name_ea_simple(name)
            function = ida_funcs.get_func(address)
            if function is None or function.start_ea != address:
                report["errors"].append(f"Function not found at a function entry: {name}")
                continue
            selected = {"request": name, "name": ida_funcs.get_func_name(address),
                        "address": hex(address), "assembly": [], "cfg": [], "pseudocode": None}
            report["selected"].append(selected)
            selected["cfg"] = [
                {"start": hex(block.start_ea), "end": hex(block.end_ea), "type": int(block.type),
                 "successors": [hex(successor.start_ea) for successor in block.succs()]}
                for block in ida_gdl.FlowChart(function, flags=0)
            ]
            for item in idautils.FuncItems(address):
                if ida_bytes.is_code(ida_bytes.get_flags(item)):
                    data = ida_bytes.get_bytes(item, ida_bytes.get_item_size(item))
                    selected["assembly"].append({
                        "address": hex(item), "bytes": data.hex() if data else "",
                        "text": ida_lines.tag_remove(idc.generate_disasm_line(item, 0) or ""),
                    })
            if not selected["assembly"]:
                report["errors"].append(f"No assembly recovered for {name}")
            if decompiler:
                try:
                    failure = ida_hexrays.hexrays_failure_t()
                    cfunc = ida_hexrays.decompile(address, failure)
                    if cfunc is None:
                        raise RuntimeError(f"{failure.desc()} at {hex(failure.errea)}")
                    selected["pseudocode"] = "\n".join(
                        ida_lines.tag_remove(line.line) for line in cfunc.get_pseudocode())
                except Exception as error:
                    report["errors"].append(f"Decompiling {name}: {type(error).__name__}: {error}")
    except Exception as error:
        report["errors"].append(f"Analysis: {type(error).__name__}: {error}")
    finally:
        if opened:
            try:
                idapro.close_database(False)
            except Exception as error:
                report["errors"].append(f"Closing database: {type(error).__name__}: {error}")
        report["status"] = "failed" if report["errors"] else "ok"
        write_report(directory, report)
    return 1 if report["errors"] else 0


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("binary", type=Path)
    parser.add_argument("--ida-dir", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True, help="new disposable output directory")
    parser.add_argument("--function", action="append", required=True, dest="functions",
                        help="function name or hexadecimal entry address (for stripped binaries)")
    parser.add_argument("--ida-user-dir", type=Path,
                        default=Path(os.environ.get("IDAUSR", Path.home() / ".idapro")),
                        help="existing profile containing accepted IDA terms (only ida.reg is copied)")
    args = parser.parse_args()
    binary, ida_dir, directory = args.binary.resolve(), args.ida_dir.resolve(), args.output.resolve()
    if not binary.is_file():
        parser.error(f"input is not a file: {binary}")
    if binary.suffix.lower() in {".idb", ".i64"}:
        parser.error("supply a native binary, not an existing IDA database")
    if not (ida_dir / "libidalib.so").is_file():
        parser.error(f"IDA library not found in {ida_dir}")
    if directory.exists():
        parser.error(f"output directory already exists: {directory}")
    try:
        package_version = importlib.metadata.version("idapro")
    except importlib.metadata.PackageNotFoundError:
        parser.error("install this IDA installation's idalib/python package into the current Python environment")
    directory.mkdir(parents=True)
    inputs = directory / "input"
    inputs.mkdir()
    copied = inputs / binary.name
    shutil.copyfile(binary, copied)
    report = {
        "schema_version": 1, "status": "starting", "input_name": binary.name,
        "input_sha256": hashlib.sha256(copied.read_bytes()).hexdigest(),
        "database_input": str(copied.relative_to(directory)),
        "requested_functions": list(dict.fromkeys(args.functions)),
        "versions": {"python": sys.version.split()[0], "idapro_package": package_version,
                     "exporter_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest()},
        "functions": [], "selected": [], "errors": [],
        "transport": "direct idalib; no MCP bridge",
        "worker_timeout_seconds": 600,
        "native_console": "suppressed to avoid exporting license/customer metadata",
        "analysis_settings": {
            "database": "fresh input copy", "flowchart_flags": 0,
            "manual_names_or_types_applied": False,
            "numeric_selectors_only": all(re.fullmatch(r"0[xX][0-9a-fA-F]+", item) for item in args.functions),
            "configuration_sha256": {
                name: hashlib.sha256((ida_dir / "cfg" / name).read_bytes()).hexdigest()
                if (ida_dir / "cfg" / name).is_file() else None
                for name in ("ida.cfg", "hexrays.cfg")
            },
        },
    }
    write_report(directory, report)
    # Import idapro first, as required by the local idalib documentation. A child
    # process also keeps native initialization failures away from the launcher.
    command = [sys.executable, "-c",
               "import idapro; import runpy; "
               f"runpy.run_path({str(Path(__file__).resolve())!r}, run_name='__main__', "
               "init_globals={'_idapython_version': IDAPYTHON_VERSION})",
               "--worker", str(directory)]
    with tempfile.TemporaryDirectory(prefix="lyra-ida-profile-") as home:
        user_dir = Path(home) / ".idapro"
        user_dir.mkdir(mode=0o700)
        registry = args.ida_user_dir.resolve() / "ida.reg"
        if registry.is_file():
            shutil.copyfile(registry, user_dir / "ida.reg")
        # Match py-activate-idalib.py's schema only in this temporary HOME.
        (user_dir / "ida-config.json").write_text(
            json.dumps({"Paths": {"ida-install-dir": str(ida_dir)}}) + "\n", encoding="utf-8")
        environment = dict(os.environ, HOME=home, IDAUSR=str(user_dir), IDADIR=str(ida_dir))
        try:
            result = subprocess.run(command, env=environment, capture_output=True, text=True,
                                    errors="replace", timeout=report["worker_timeout_seconds"])
        except subprocess.TimeoutExpired:
            # subprocess.run kills and waits for the worker before raising.
            # Leaving this context also removes the private copied profile.
            report = json.loads((directory / "analysis.json").read_text(encoding="utf-8"))
            report["status"] = "failed"
            report["worker_exit_status"] = None
            report["errors"].append(
                f"IDA worker exceeded timeout of {report['worker_timeout_seconds']} seconds; killed and reaped")
            write_report(directory, report)
            print(f"IDA export failed: {directory / 'analysis.json'}")
            return 1
    report = json.loads((directory / "analysis.json").read_text(encoding="utf-8"))
    if result.returncode or report["status"] != "ok":
        report["status"] = "failed"
        report["errors"].append(f"IDA worker exited with status {result.returncode}")
        # Retain only narrowly recognized bootstrap diagnostics, never raw IDA
        # output (which may include license identities or customer information).
        for line in (result.stdout + "\n" + result.stderr).splitlines():
            if line == "License not yet accepted, cannot run in batch mode":
                report["errors"].append(line)
            match = re.fullmatch(r"ModuleNotFoundError: No module named '[A-Za-z0-9_.]+'", line)
            init_error = re.search(r"init_library error code -?\d+", line)
            if match:
                report["errors"].append(match.group())
            elif init_error:
                report["errors"].append(init_error.group())
        if not report["versions"].get("idalib"):
            report["errors"].append(
                "IDA did not initialize; check Python compatibility, shared-library dependencies, "
                "local license availability, and prior acceptance of IDA terms. "
                "Native console diagnostics were not exported.")
    report["worker_exit_status"] = result.returncode
    write_report(directory, report)
    print(f"IDA export {report['status']}: {directory / 'analysis.json'}")
    return 0 if report["status"] == "ok" else 1


if __name__ == "__main__":
    if len(sys.argv) == 3 and sys.argv[1] == "--worker":
        sys.exit(analyze(Path(sys.argv[2])))
    sys.exit(main())
