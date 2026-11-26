#!/usr/bin/env python3
# Copyright (c) 2026 Hakai Offensive Security.
# SPDX-License-Identifier: GPL-3.0-or-later
"""Measure existing native pass profiles using the CI runner and public fixtures.

Builds use cold target directories, warm/offline Cargo downloads, LTO off and
one codegen unit, matching ci_smoke.py. Runtime samples use warmups and one
available CPU where affinity is supported. No OS caches or governors are changed.
Native work_ns excludes process startup; process wall time includes spawning,
loading, argument parsing and output. The cdylib probe also includes Python and
ABI checks, so its wall time is not isolated dlopen or function-call latency.
"""

import argparse
from contextlib import contextmanager
import hashlib
import json
import os
from pathlib import Path
import platform
import random
import shutil
import statistics
import sys

from benchmark_workloads import WORKLOADS, expected_checksum
from ci_smoke import CANARIES, EXE, FLAGS, ROOT, Runner, require


PROFILES = {"plain": None, "no-passes": [], **{flag[2:]: [flag] for flag in FLAGS},
            "branch-mba": ["--indirect-branch", "--mba"],
            "call-mba": ["--indirect-call", "--mba"],
            "outline-call": ["--block-outline", "--indirect-call"], "all-passes": FLAGS}
INSPECT_PROFILES = {"plain", "no-passes", "mba", "branch-mba", "indirect-call", "call-mba",
                    "block-outline", "outline-call", "all-passes"}
SYMBOLS = {"test_project": ["bench_integer_step", "bench_text", "bench_fft"],
           "test_dll_project": ["add", "float_add_f64"]}


def save(path, data):
    path.write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8")


def sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def summarize(samples):
    require(bool(samples), "cannot summarize missing measurements")
    return {"unit": "seconds", "samples": samples, "median": statistics.median(samples),
            "mean": statistics.mean(samples), "min": min(samples), "max": max(samples),
            "sample_stdev": statistics.stdev(samples) if len(samples) > 1 else None}


def command_seconds(directory, label):
    return json.loads((directory / f"{label}.command.json").read_text(encoding="utf-8"))["seconds"]


@contextmanager
def runtime_affinity(cpu):
    if cpu is None:
        yield
        return
    previous = os.sched_getaffinity(0)
    os.sched_setaffinity(0, {cpu})
    try:
        yield
    finally:
        os.sched_setaffinity(0, previous)


def provenance(runner, directory, args):
    revision, _ = runner.run(directory, "revision", ["git", "rev-parse", "HEAD"], capture=True)
    runner.run(directory, "working-patch", ["git", "diff", "--no-ext-diff", "--no-textconv", "HEAD"])
    sources = [ROOT / "Cargo.toml", ROOT / "Cargo.lock", *sorted((ROOT / "src").rglob("*.rs"))]
    sources += [ROOT / "scripts" / name for name in
                ("ci_smoke.py", "measure_passes.py", "benchmark_workloads.py", "probe_cdylib.py", "export_ida.py")]
    hashes = {}
    for source in sources:
        relative = source.relative_to(ROOT)
        destination = directory / "source" / relative
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source, destination)
        hashes[str(relative)] = sha256(destination)
    for name in ("llvm-size", "llvm-nm", "llvm-strip"):
        text, _ = runner.run(directory, f"{name}-version", [runner.llvm_bin / (name + EXE), "--version"],
                             timeout=30, capture=True)
        runner.report["versions"]["llvm"][name] = text.decode("utf-8")
    affinity = sorted(os.sched_getaffinity(0)) if hasattr(os, "sched_getaffinity") else None
    cpu = affinity[0] if affinity else None
    cpu_model = None
    cpuinfo = Path("/proc/cpuinfo")
    if cpuinfo.is_file():
        cpu_model = next((line.split(":", 1)[1].strip() for line in cpuinfo.read_text().splitlines()
                          if line.startswith("model name")), None)
    governor = Path(f"/sys/devices/system/cpu/cpu{cpu}/cpufreq/scaling_governor") if cpu is not None else None
    runner.report["measurement"] = {
        "schema_version": 1, "revision": revision.decode().strip(), "source_sha256": hashes,
        "lyra_binary": str(runner.lyra), "lyra_sha256": sha256(runner.lyra),
        "python": sys.version, "platform": platform.platform(),
        "cpu_model": cpu_model, "build_affinity": affinity, "runtime_cpu": cpu,
        "governor": governor.read_text().strip() if governor and governor.is_file() else None,
        "load_average_start": list(os.getloadavg()) if hasattr(os, "getloadavg") else None,
        "policy": {"target_cache": "cleaned before each build; cleanup excluded from timing",
                   "cargo_download_cache": "warm, offline", "os_page_cache": "not flushed; runtime warmups",
                   "lto": "off", "codegen_units": 1,
                   "build_scope": "end-to-end cargo or Lyra invocation; coverage disabled",
                   "coverage_scope": "separate untimed build with identical project, flags and seed",
                   "runtime_scope": "native work_ns and process wall reported separately",
                   "cdylib_scope": "whole Python loader/ABI probe, not isolated load or call latency",
                   "comparison": "controlled compiler settings, not stock project LTO defaults",
                   "ida_scope": "fresh stripped copies, numeric entry addresses, no source names/types supplied"},
        "config": {"runs": args.runs, "warmups": args.warmups, "build_runs": args.build_runs,
                   "seeds": args.seeds, "order_seed": args.order_seed, "profiles": args.profiles,
                   "fixtures": args.fixtures, "workloads": WORKLOADS},
        "rows": [],
    }
    return cpu


def coverage_records(stdout, flags):
    records = [json.loads(line.removeprefix("LYRA_COVERAGE "))
               for line in stdout.decode("utf-8").split("\n") if line.startswith("LYRA_COVERAGE ")]
    expected = {flag[2:] for flag in flags}
    require({record["pass"] for record in records} == expected, "missing or unexpected pass coverage records")
    for record in records:
        require(record["schema_version"] == 1 and bool(record["unit"]), "unsupported coverage schema/unit")
        require(record.get("complete", True), "partial coverage record from an aborted pass")
        for counts in [record, *record.get("rounds", [])]:
            require(all(type(counts[key]) is int and counts[key] >= 0
                        for key in ("considered", "eligible", "transformed", "skipped")), "invalid coverage count")
            require(counts["transformed"] <= counts["eligible"] <= counts["considered"], "invalid eligibility totals")
            require(counts["considered"] == counts["transformed"] + counts["skipped"], "invalid coverage partition")
            require(all(type(value) is int and value >= 0 for value in counts["skip_reasons"].values()),
                    "invalid coverage skip reasons")
            require(sum(counts["skip_reasons"].values()) == counts["skipped"], "skip reasons do not account for skips")
    return records


def section_sizes(runner, directory, label, artifact):
    text, _ = runner.run(directory, label, [runner.llvm_bin / ("llvm-size" + EXE),
                        "--format=sysv", "--radix=10", artifact], timeout=30, capture=True)
    sections = {}
    for line in text.decode("utf-8").splitlines():
        fields = line.split()
        if len(fields) == 3 and fields[1].isdigit() and fields[2].isdigit():
            sections[fields[0]] = int(fields[1])
    require(bool(sections), "llvm-size produced no recognizable section sizes")
    return {"file_bytes": artifact.stat().st_size, "section_bytes": sections,
            "section_total_bytes": sum(sections.values()), "sha256": sha256(artifact)}


def artifact_name(fixture):
    if fixture == "test_project":
        return "test_project" + EXE
    return "test_dll.dll" if os.name == "nt" else "libtest_dll.dylib" if sys.platform == "darwin" else "libtest_dll.so"


def build(runner, directory, project, fixture, flags, seed, *, coverage=False):
    directory.mkdir(parents=True)
    runner.run(directory, "clean", ["cargo", "clean"], cwd=project, capture=True)
    artifact = directory / artifact_name(fixture)
    if flags is None:
        runner.run(directory, "build", ["cargo", "build", "--locked", "--release", "--target", runner.target,
                   *( ["--bin", "test_project"] if fixture == "test_project" else ["--lib"])],
                   cwd=project, capture=True)
        shutil.copy2(project / "target" / runner.target / "release" / artifact.name, artifact)
        stdout = b""
    else:
        command = [runner.lyra, "--project", project, "--output", artifact, "--target", runner.target,
                   "--crate-type", "bin" if fixture == "test_project" else "cdylib",
                   "--seed", str(seed), "--keep-temps", *flags]
        if fixture == "test_project":
            command += ["--bin", "test_project"]
        if coverage:
            runner.env["LYRA_COVERAGE"] = "1"
        try:
            stdout, _ = runner.run(directory, "build", command, cwd=project, capture=True)
        finally:
            runner.env.pop("LYRA_COVERAGE", None)
    seconds = command_seconds(directory, "build")
    runner.execute(directory, artifact, fixture)
    require((CANARIES[fixture] not in artifact.read_bytes()) if flags and "--string-enc" in flags
            else (CANARIES[fixture] in artifact.read_bytes()), "artifact plaintext canary check failed")
    ir_hashes = {}
    if flags is not None:
        transformed = sorted((directory / "tmp").rglob("*.obf.ll"))
        require(bool(transformed), "no transformed IR retained")
        ir_dir = directory / "ir"
        ir_dir.mkdir()
        for index, path in enumerate(transformed):
            runner.run(directory, f"verify-{index}", [runner.llvm_bin / ("opt" + EXE),
                       "--non-global-value-max-name-size=-1", "-passes=verify", "-disable-output", path], timeout=60)
            name = f"transformed-{index}-{path.name}"
            shutil.copy2(path, ir_dir / name)
            ir_hashes[name] = sha256(path)
        for index, path in enumerate(sorted((project / "target").rglob("*.ll"))):
            shutil.copy2(path, ir_dir / f"original-{index}-{path.name}")
    # Retain binaries and original/transformed IR, not all duplicated linker temps.
    shutil.rmtree(directory / "tmp")
    records = coverage_records(stdout, flags) if coverage else []
    return artifact, {"seconds": seconds, "artifact": str(artifact.relative_to(runner.output)),
                      "size": section_sizes(runner, directory, "sections", artifact),
                      "transformed_ir_sha256": ir_hashes, "coverage": records}


def runtime_samples(runner, directory, artifact, fixture, cpu, args, expected):
    directory.mkdir()
    results = {}
    workloads = WORKLOADS if fixture == "test_project" else [
        {"name": "cdylib-abi-probe", "description": "Python startup, dlopen, greeting and typed ABI checks"}]
    with runtime_affinity(cpu):
        for workload in workloads:
            name = workload["name"]
            case = directory / name
            case.mkdir()
            wall, work, warmups = [], [], []
            for index in range(args.warmups + args.runs):
                label = f"sample-{index}"
                if fixture == "test_project":
                    command = [artifact, "--bench", name, str(workload["iterations"]), str(workload["seed"])]
                else:
                    command = [sys.executable, ROOT / "scripts/probe_cdylib.py", artifact]
                stdout, stderr = runner.run(case, label, command, cwd=case, timeout=120, capture=True)
                require(stderr == b"", f"{name} wrote stderr: {stderr!r}")
                body_seconds = None
                if fixture == "test_project":
                    result = json.loads(stdout)
                    require(result["workload"] == name and result["iterations"] == workload["iterations"]
                            and result["seed"] == workload["seed"], "benchmark input identity mismatch")
                    require(result["checksum"] == expected[name],
                            f"{name} checksum: expected {expected[name]}, got {result['checksum']}")
                    if name != "startup":
                        require(type(result["work_ns"]) is int and result["work_ns"] >= 0, "invalid native work duration")
                        body_seconds = result["work_ns"] / 1e9
                else:
                    require(stdout.replace(b"\r\n", b"\n") == b"cdylib ABI and loader-time greeting passed\n",
                            "cdylib probe output mismatch")
                seconds = command_seconds(case, label)
                if index < args.warmups:
                    warmups.append({"process_wall_seconds": seconds, "native_work_seconds": body_seconds})
                else:
                    wall.append(seconds)
                    if body_seconds is not None:
                        work.append(body_seconds)
            results[name] = {"process_wall": summarize(wall), "warmups": warmups,
                             "native_work": summarize(work) if work else None,
                             "expected_checksum": expected.get(name), "description": workload["description"]}
    return results


def prepare_inspection(runner, directory, artifact, fixture):
    directory.mkdir()
    runner.run(directory, "symbols", [runner.llvm_bin / ("llvm-nm" + EXE), "--defined-only",
               "--print-size", "--format=posix", "--radix=x", artifact], timeout=30)
    correspondence = []
    for line in (directory / "symbols.stdout.log").read_text(encoding="utf-8").splitlines():
        fields = line.split()
        if len(fields) >= 3 and fields[0].removeprefix("_") in SYMBOLS[fixture] and fields[1].upper() == "T":
            correspondence.append({"source_symbol": fields[0], "address": hex(int(fields[2], 16))})
    require(len(correspondence) == len(SYMBOLS[fixture]), "missing inspection entry symbols")
    # Source correspondence is deliberately outside the stripped input directory.
    save(directory / "source-correspondence.json", correspondence)
    inputs = directory / "stripped"
    inputs.mkdir()
    stripped = inputs / artifact.name
    runner.run(directory, "strip", [runner.llvm_bin / ("llvm-strip" + EXE), "--strip-all", "-o", stripped, artifact])
    shutil.copymode(artifact, stripped)
    runner.execute(directory, stripped, fixture)
    return {"artifact": str(stripped.relative_to(runner.output)), "size": section_sizes(runner, directory, "sections", stripped),
            "strip_mode": "--strip-all; dynamic ABI exports may remain", "entries": correspondence}


def inspect_with_ida(runner, directory, inspection, args):
    output = directory / "ida"
    command = [args.ida_python, ROOT / "scripts/export_ida.py", runner.output / inspection["artifact"],
               "--ida-dir", args.ida_dir, "--output", output]
    for entry in inspection["entries"]:
        command += ["--function", entry["address"]]
    try:
        runner.run(directory, "ida-export", command, timeout=630, capture=True)
    except RuntimeError:
        # Decompiler failure is measured data, not runtime correctness failure.
        # Infrastructure failures without a real report still fail this case.
        if not (output / "analysis.json").is_file():
            raise
    report = json.loads((output / "analysis.json").read_text(encoding="utf-8"))
    require(report["input_sha256"] == inspection["size"]["sha256"], "IDA report describes a different artifact")
    require("processor" in report, f"IDA failed before artifact analysis; see {output / 'analysis.json'}")
    requested = len(inspection["entries"])
    located = len(report["selected"])
    decompiled = sum(item["pseudocode"] is not None for item in report["selected"])
    print(f"IDA {report['status'].upper()} (recorded): {located}/{requested} entries located, "
          f"{decompiled}/{requested} pseudocode results; {output / 'analysis.json'}", flush=True)
    return {"status": report["status"], "errors": report["errors"], "versions": report["versions"],
            "report": str((output / "analysis.json").relative_to(runner.output)),
            "functions_recovered": len(report["functions"]), "manual_intervention": False,
            "requested_entries": requested, "located_entries": located, "pseudocode_results": decompiled,
            "analysis_wall_seconds": command_seconds(directory, "ida-export"),
            "settings": report["analysis_settings"], "worker_exit_status": report["worker_exit_status"],
            "selected": [{"address": item["address"], "instructions": len(item["assembly"]),
                          "basic_blocks": len(item["cfg"]),
                          "edges": sum(len(block["successors"]) for block in item["cfg"]),
                          "decompiled": item["pseudocode"] is not None} for item in report["selected"]]}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True, help="new absolute evidence directory")
    parser.add_argument("--lyra", type=Path, default=ROOT / "target/release" / ("lyra" + EXE))
    parser.add_argument("--llvm-bin", type=Path)
    parser.add_argument("--fixtures", nargs="+", choices=list(CANARIES), default=list(CANARIES))
    parser.add_argument("--profiles", nargs="+", choices=list(PROFILES), default=list(PROFILES))
    parser.add_argument("--seeds", nargs="+", type=int, default=[0, 1, 42])
    parser.add_argument("--build-runs", type=int, default=3)
    parser.add_argument("--runs", type=int, default=9)
    parser.add_argument("--warmups", type=int, default=2)
    parser.add_argument("--order-seed", type=int, default=0)
    parser.add_argument("--ida-python", type=Path)
    parser.add_argument("--ida-dir", type=Path)
    args = parser.parse_args()
    if not args.output.is_absolute() or args.output.exists():
        parser.error("--output must be an absolute, previously nonexistent directory")
    args.output = args.output.resolve()
    if any(args.output.is_relative_to(ROOT / fixture) for fixture in CANARIES):
        parser.error("--output must not be inside a source fixture")
    if args.runs < 1 or args.build_runs < 1 or args.warmups < 0 or any(not 0 <= seed < 2**64 for seed in args.seeds):
        parser.error("runs/build-runs must be positive, warmups nonnegative, and seeds u64")
    if bool(args.ida_python) != bool(args.ida_dir):
        parser.error("--ida-python and --ida-dir must be supplied together")
    args.profiles, args.fixtures, args.seeds = (list(dict.fromkeys(values)) for values in
                                               (args.profiles, args.fixtures, args.seeds))
    if args.ida_python:
        # Resolving a venv interpreter symlink would lose its environment.
        args.ida_python, args.ida_dir = args.ida_python.absolute(), args.ida_dir.resolve()
    configured = args.llvm_bin or os.environ.get("LYRA_LLVM_BIN")
    if not configured and os.environ.get("LLVM_SYS_221_PREFIX"):
        configured = Path(os.environ["LLVM_SYS_221_PREFIX"]) / "bin"
    if not configured:
        parser.error("set --llvm-bin, LYRA_LLVM_BIN, or LLVM_SYS_221_PREFIX")
    args.output.mkdir(parents=True)
    runner = Runner(args.output, Path(configured).resolve(), args.lyra.resolve())
    inspections = []
    try:
        if not runner.case("preflight", runner.preflight):
            return 1
        state = {}
        if not runner.case("provenance", lambda directory: state.update(cpu=provenance(runner, directory, args))):
            return 1
        expected = {w["name"]: expected_checksum(w["name"], w["iterations"], w["seed"]) for w in WORKLOADS}
        runner.report["measurement"]["expected_checksums"] = expected
        order = random.Random(args.order_seed)
        for fixture in args.fixtures:
            project = args.output / "projects" / fixture / "project with spaces"
            shutil.copytree(ROOT / fixture, project, ignore=shutil.ignore_patterns("target", ".git"))
            runner.report["measurement"].setdefault("inputs", {})[fixture] = {
                "directory": str(project.relative_to(args.output)),
                "sha256": {str(path.relative_to(project)): sha256(path)
                           for path in sorted(project.rglob("*")) if path.is_file()},
            }
            matrix = [(profile, seed) for profile in args.profiles for seed in
                      ([None] if profile == "plain" else args.seeds)]
            order.shuffle(matrix)
            for profile, seed in matrix:
                flags = PROFILES[profile]
                name = f"{fixture}/{profile}/seed-{seed if seed is not None else 'none'}"
                row = {"fixture": fixture, "profile": profile, "seed": seed, "flags": flags,
                       "status": "running", "case": name, "builds": []}
                runner.report["measurement"]["rows"].append(row)

                def measure(directory):
                    for repeat in range(args.build_runs):
                        artifact, measured = build(runner, directory / f"build-{repeat}", project, fixture, flags, seed)
                        row["builds"].append(measured)
                    row["build_wall"] = summarize([item["seconds"] for item in row["builds"]])
                    row["runtime_artifact"] = str(artifact.relative_to(runner.output))
                    row["observed_identical_build_hashes"] = len({item["size"]["sha256"] for item in row["builds"]}) == 1
                    if flags:
                        _, coverage = build(runner, directory / "coverage", project, fixture, flags, seed, coverage=True)
                        require(coverage["transformed_ir_sha256"] == row["builds"][-1]["transformed_ir_sha256"],
                                "coverage collection changed emitted IR for the same inputs/seed")
                        row["coverage"] = coverage
                    else:
                        row["coverage"] = None
                    row["runtime"] = runtime_samples(runner, directory / "runtime", artifact, fixture, state["cpu"], args, expected)
                    row["stripped"] = prepare_inspection(runner, directory / "inspection", artifact, fixture)
                    if args.ida_python and profile in INSPECT_PROFILES and seed in (None, 0):
                        inspections.append(row)

                row["status"] = "passed" if runner.case(name, measure) else "failed"
                runner.save_report()
            shutil.rmtree(project / "target", ignore_errors=True)
        # Keep analysis tool load out of all measured build/runtime phases.
        for row in inspections:
            def analyze(case):
                row["ida"] = inspect_with_ida(runner, case, row["stripped"], args)
            runner.case(row["case"] + "/ida-check", analyze)
        rows = runner.report["measurement"]["rows"]
        for row in rows:
            baseline = next((base for base in rows if base["fixture"] == row["fixture"]
                             and base["profile"] == "plain" and base["status"] == "passed"), None)
            if row["status"] != "passed" or baseline is None:
                continue
            row["ratios_to_plain"] = {
                "build_wall_median": row["build_wall"]["median"] / baseline["build_wall"]["median"],
                "file_bytes": row["builds"][-1]["size"]["file_bytes"] / baseline["builds"][-1]["size"]["file_bytes"],
                "stripped_file_bytes": row["stripped"]["size"]["file_bytes"] / baseline["stripped"]["size"]["file_bytes"],
                "process_wall_medians": {name: data["process_wall"]["median"] / baseline["runtime"][name]["process_wall"]["median"]
                                         for name, data in row["runtime"].items()},
                "native_work_medians": {name: data["native_work"]["median"] / baseline["runtime"][name]["native_work"]["median"]
                                        for name, data in row["runtime"].items() if data["native_work"]
                                        and baseline["runtime"][name]["native_work"]["median"] > 0},
            }
        runner.report["measurement"]["load_average_end"] = list(os.getloadavg()) if hasattr(os, "getloadavg") else None
    finally:
        runner.save_report()
    return int(any(result["status"] == "failed" for result in runner.report["results"]))


if __name__ == "__main__":
    sys.exit(main())
