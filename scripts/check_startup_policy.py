#!/usr/bin/env python3
"""Exercise initialization-safe string encryption in real Linux processes.

Requires LLVM 22, Zig and Docker. The fixtures are Linux ABI probes even when
this command runs on macOS. This does not replace Windows ImageAttach/JVM smoke.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess

from ci_smoke import FLAGS, ROOT, clean_environment, require


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', required=True, type=Path, help='new evidence directory')
    parser.add_argument('--lyra', type=Path, default=ROOT / 'target/release/lyra')
    parser.add_argument('--llvm-bin', type=Path, default=os.environ.get('LYRA_LLVM_BIN'))
    args = parser.parse_args()
    if args.llvm_bin is None:
        parser.error('set LYRA_LLVM_BIN or pass --llvm-bin for LLVM 22')
    llvm = args.llvm_bin.resolve()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    executable = args.lyra.resolve()
    environment = clean_environment(llvm)
    environment.update(LYRA_SEED='0', LYRA_COVERAGE='1')
    results = []
    for fixture in ['string_tls', 'string_preinit']:
        original = output / (fixture + '.ll')
        subprocess.run([str(llvm / 'clang'), '--target=x86_64-unknown-linux-gnu', '-O1',
                        '-emit-llvm', '-S', str(ROOT / 'tests' / (fixture + '.c')),
                        '-o', str(original)], check=True, env=environment)
        for profile, flags in [('baseline', []), ('string-enc', ['--string-enc']), ('all-six', FLAGS)]:
            prefix = output / (fixture + '-' + profile)
            ir = original
            records = []
            if flags:
                ir = prefix.with_suffix('.ll')
                log = subprocess.check_output([str(executable), '__obfuscate-ir', str(original),
                                               str(ir), *flags], env=environment, text=True)
                prefix.with_suffix('.log').write_text(log)
                records = [json.loads(line.removeprefix('LYRA_COVERAGE '))
                           for line in log.splitlines() if line.startswith('LYRA_COVERAGE ')]
                row = next(row for row in records if row['pass'] == 'string-enc')
                require(type(row.get('initialization_policy')) is int and row['initialization_policy'] == 1,
                        'compiler does not implement initialization_policy=1')
                reason = 'thread_local' if fixture == 'string_tls' else 'preinit_dependency'
                require(row['skip_reasons'].get(reason, 0) > 0, 'startup dependency was not excluded')
                if fixture == 'string_preinit':
                    require(row['transformed'] > 0 and 'encrypt-after-runtime' not in ir.read_text(),
                            'ordinary post-startup string was not encrypted')
            obj = prefix.with_suffix('.o')
            subprocess.run([str(llvm / 'llc'), '-filetype=obj', '-relocation-model=pic', str(ir),
                            '-o', str(obj)], check=True, env=environment)
            subprocess.run(['zig', 'cc', '-target', 'x86_64-linux-gnu.2.17', str(obj), '-pthread',
                            '-o', str(prefix)], check=True, env=environment)
            # Execute on the container filesystem, not Docker Desktop's host
            # bind mount (the identical plain ELF faulted only on that mount).
            subprocess.run(['docker', 'run', '--rm', '--platform', 'linux/amd64',
                            '-v', str(output) + ':/checks:ro', 'debian:bookworm-slim',
                            'sh', '-c', 'cp "$1" /tmp/startup-oracle && exec /tmp/startup-oracle',
                            'sh', '/checks/' + prefix.name], check=True, timeout=30)
            results.append({'fixture': fixture, 'profile': profile, 'exit': 0, 'coverage': records})
            print(f'{fixture}/{profile}: passed', flush=True)
    report = {'lyra': str(executable), 'sha256': hashlib.sha256(executable.read_bytes()).hexdigest(),
              'results': results}
    (output / 'report.json').write_text(json.dumps(report, indent=2))


if __name__ == '__main__':
    main()
