#!/usr/bin/env python3
"""Check PR titles and commit subjects from the GitHub event and local history."""
import json
import os
from pathlib import Path
import re
import subprocess
import sys


EXPECTED = 'fix(ci): <short description>'
SUBJECT = re.compile(r'fix\(ci\): \S(?:[^\r\n]*\S)?')
SHA = re.compile(r'(?:[0-9a-fA-F]{40}|[0-9a-fA-F]{64})')


def git(*args):
    result = subprocess.run(
        ['git', '--no-pager', '-c', 'log.showSignature=false', *args],
        check=True, capture_output=True, text=True, encoding='utf-8', errors='replace',
    )
    return result.stdout.strip()


def revision(value):
    if not isinstance(value, str) or not SHA.fullmatch(value):
        raise ValueError('event must contain a full commit SHA')
    return value


def check_subject(value, label):
    if not isinstance(value, str) or not SUBJECT.fullmatch(value):
        print(f'error: {label} must use {EXPECTED}', file=sys.stderr)
        return False
    return True


def main():
    event_path = os.environ.get('GITHUB_EVENT_PATH')
    event_name = os.environ.get('GITHUB_EVENT_NAME')
    if not event_path or not event_name:
        raise ValueError('set GITHUB_EVENT_PATH and GITHUB_EVENT_NAME')
    event = json.loads(Path(event_path).read_text(encoding='utf-8'))
    if not isinstance(event, dict):
        raise ValueError('GitHub event must be a JSON object')

    if event_name == 'pull_request':
        pr = event['pull_request']
        valid = check_subject(pr['title'], 'PR title')
        base, head = revision(pr['base']['sha']), revision(pr['head']['sha'])
        if git('rev-parse', '--is-shallow-repository') != 'false':
            raise ValueError('PR policy requires full history; use checkout fetch-depth: 0')
        git('cat-file', '-e', base + '^{commit}')
        git('cat-file', '-e', head + '^{commit}')
        commits = git('log', '--format=%H%x00%s', base + '..' + head, '--')
    elif event_name == 'push':
        head = revision(event['after'])
        if set(head) == {'0'}:
            print('Contribution policy: deleted ref; no commit to check.')
            return 0
        valid = True
        commits = git('log', '-1', '--format=%H%x00%s', head, '--')
    else:
        print('Contribution policy: no PR or push to check.')
        return 0

    count = 0
    for commit in commits.splitlines():
        sha, subject = commit.split('\0', 1)
        valid = check_subject(subject, 'commit ' + sha[:12]) and valid
        count += 1
    if valid:
        print(f'Contribution policy: checked {count} commit subject(s).')
    return 0 if valid else 1


if __name__ == '__main__':
    try:
        sys.exit(main())
    except (OSError, ValueError, KeyError, TypeError, subprocess.CalledProcessError):
        print(
            'error: contribution policy could not read the event or commit history; '
            'set GITHUB_EVENT_PATH and GITHUB_EVENT_NAME and provide full history '
            f'with both PR endpoints. Expected title and subject: {EXPECTED}',
            file=sys.stderr,
        )
        sys.exit(1)
