#!/usr/bin/env python3
"""Check indexed content, including files force-added despite .gitignore."""

import hashlib
from pathlib import PurePosixPath
import re
import subprocess
import sys


TEST_KEY = "tests/fixtures/test-key.pem"
TEST_KEY_SHA256 = "2b87b2f661c668962deacfdfd42026b5b1e50f6d1f67004e752a586c7436ea8e"
PRIVATE_KEY = re.compile(rb"-----BEGIN (?:RSA |EC |OPENSSH |DSA |ENCRYPTED )?PRIVATE KEY-----")
JWT = re.compile(rb"\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\b")
DATABASE_PASSWORD = re.compile(rb"postgres(?:ql)?://[^\s:/@]+:([^\s/@]+)@")
EXAMPLE_PASSWORDS = {b"password", b"change-me", b"sumi-ci"}
FORBIDDEN_SUFFIXES = {".pem", ".key", ".p12", ".pfx", ".dump", ".backup", ".db", ".sqlite", ".sqlite3", ".eml", ".mbox"}
FORBIDDEN_NAMES = {"email.json", "ledger-export.json", "mcp-transcript.json", ".DS_Store"}


def main():
    try:
        indexed = subprocess.check_output(["git", "ls-files", "--stage", "-z"], stderr=subprocess.PIPE)
    except subprocess.CalledProcessError:
        print("Run inside a Git repository after staging the intended files.", file=sys.stderr)
        return 2
    entries = [item for item in indexed.split(b"\0") if item]
    if not entries:
        print("No indexed files; stage the intended files before checking.", file=sys.stderr)
        return 2
    issues = []
    for entry in entries:
        metadata, raw_path = entry.split(b"\t", 1)
        mode, oid, stage = metadata.split()
        name = raw_path.decode("utf-8", errors="surrogateescape")
        path = PurePosixPath(name)
        if stage != b"0":
            issues.append((name, "unresolved merge entry"))
            continue
        forbidden = (
            bool({".local", "target", "backups", "pgdata"}.intersection(path.parts))
            or ((path.name == ".env" or path.name.startswith(".env.")) and name != ".env.example")
            or (path.suffix.lower() in FORBIDDEN_SUFFIXES and name != TEST_KEY)
            or path.name in FORBIDDEN_NAMES
            or path.name.endswith((".sql.gz", ".log"))
        )
        if forbidden:
            issues.append((name, "local data, backup or credential path must not be tracked"))
            continue
        if mode == b"160000":
            continue
        content = subprocess.check_output(["git", "cat-file", "blob", oid.decode("ascii")])
        if name == TEST_KEY:
            if hashlib.sha256(content).hexdigest() != TEST_KEY_SHA256:
                issues.append((name, "test key differs from the approved public fixture"))
            continue
        if PRIVATE_KEY.search(content):
            issues.append((name, "unapproved private key content"))
        if JWT.search(content):
            issues.append((name, "literal JWT; generate test tokens at runtime"))
        if any(match.group(1) not in EXAMPLE_PASSWORDS for match in DATABASE_PASSWORD.finditer(content)):
            issues.append((name, "database URL contains a non-example password"))
    if issues:
        for name, reason in issues:
            print(f"{name!r}: {reason}", file=sys.stderr)
        print("Repository content check failed. Remove these entries from the index.", file=sys.stderr)
        return 1
    print(f"Checked {len(entries)} indexed files: local data and credential checks passed.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
