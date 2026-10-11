#!/usr/bin/env python3
"""Check publication candidates and reachable history without printing sensitive values.

Heuristic checks complement human review. This never scans outside the named repo.
"""
import argparse
import json
from pathlib import Path
import re
import subprocess

RULES = {
    'private-key': rb'-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----',
    'github-token': rb'gh[pousr]_[A-Za-z0-9]{30,}|github_pat_[A-Za-z0-9_]{40,}',
    'cloud-access-key': rb'\b(?:AKIA|ASIA)[A-Z0-9]{16}\b',
    'api-secret': rb'\bsk-(?:proj-)?[A-Za-z0-9_-]{32,}',
    'personal-absolute-path': rb'/(?:home|Users)/[A-Za-z0-9_.-]+/',
    'ai-coauthor': (rb'(?im)^co' + rb'-authored-by:\s*[^\n]*(?:clau' + rb'de|anthro' + rb'pic|openai|codex)'),
    'legal-case-number': rb'\b\d{7}-\d{2}\.\d{4}\.\d\.\d{2}\.\d{4}\b',
    'brazilian-person-id': rb'\b\d{3}\.\d{3}\.\d{3}-\d{2}\b',
}
PATTERNS = {key: re.compile(value) for key, value in RULES.items()}
AI_IDENTITY = re.compile(rb'(?i)\b(?:clau' + rb'de|anthro' + rb'pic|codex|openai)\b')


def git(root, *args):
    return subprocess.check_output(['git', '-C', str(root), *args])


def matches(data):
    return [name for name, pattern in PATTERNS.items() if pattern.search(data)]


def scan(root, history):
    findings = []
    candidates = git(root, 'ls-files', '--cached', '--others', '--exclude-standard', '-z').split(b'\0')
    files = 0
    for entry in candidates:
        if not entry:
            continue
        path = root / entry.decode()
        if not path.is_file():
            continue
        files += 1
        for rule in matches(path.read_bytes()):
            findings.append({'scope': 'working-tree', 'path': entry.decode(), 'rule': rule})
    commits = blobs = 0
    if history:
        for commit in git(root, 'rev-list', '--all').splitlines():
            commits += 1
            identities = git(root, 'show', '-s', '--format=%an%n%ae%n%cn%n%ce', commit.decode())
            if AI_IDENTITY.search(identities):
                findings.append({'scope': 'commit-identity', 'object': commit.decode()[:12],
                                 'rule': 'ai-author-or-committer'})
            message = git(root, 'show', '-s', '--format=%B', commit.decode())
            for rule in matches(message):
                findings.append({'scope': 'commit-message', 'object': commit.decode()[:12], 'rule': rule})
        objects = git(root, 'rev-list', '--objects', '--all').splitlines()
        proc = subprocess.Popen(['git', '-C', str(root), 'cat-file', '--batch'], stdin=subprocess.PIPE, stdout=subprocess.PIPE)
        try:
            for entry in objects:
                object_id = entry.split(b' ', 1)[0]
                proc.stdin.write(object_id + b'\n')
                proc.stdin.flush()
                header = proc.stdout.readline().split()
                size = int(header[2])
                data = proc.stdout.read(size)
                assert proc.stdout.read(1) == b'\n'
                if header[1] != b'blob':
                    continue
                blobs += 1
                for rule in matches(data):
                    findings.append({'scope': 'historical-blob', 'object': object_id.decode()[:12], 'rule': rule})
        finally:
            proc.stdin.close()
            proc.wait()
    return {'candidate_files': files, 'reachable_commits': commits, 'historical_blobs': blobs,
            'findings': findings, 'human_review_required': ['screenshots and other images', 'author identities', 'unrecognised confidential context']}


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('repository', nargs='?', default='.')
    parser.add_argument('--history', action='store_true')
    args = parser.parse_args()
    report = scan(Path(args.repository).resolve(), args.history)
    print(json.dumps(report, indent=2))
    raise SystemExit(bool(report['findings']))
