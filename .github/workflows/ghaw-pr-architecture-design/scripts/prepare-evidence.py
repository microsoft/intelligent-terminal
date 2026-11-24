"""Export immutable Git evidence as inert files, never as a candidate checkout."""

import json
import subprocess
import sys
from pathlib import Path


def git(*args):
    return subprocess.check_output(["git", "--no-replace-objects", *args])


def export_revision(revision, index, blobs, batch, inspected):
    for entry in filter(None, git("ls-tree", "-rlz", revision).split(b"\0")):
        header, path = entry.split(b"\t", 1)
        mode, kind, oid, size = header.decode("ascii").split()
        record = {"path": path.decode("utf-8", errors="surrogateescape")}
        filename = blobs / f"{oid}.txt"
        if mode in ("100644", "100755") and kind == "blob" and int(size) <= 1024 * 1024:
            if oid not in inspected:
                batch.stdin.write(oid.encode("ascii") + b"\n")
                batch.stdin.flush()
                if batch.stdout.readline().strip() != f"{oid} blob {size}".encode("ascii"):
                    raise ValueError(f"Unexpected Git response for blob {oid}")
                data = batch.stdout.read(int(size))
                if len(data) != int(size) or batch.stdout.read(1) != b"\n":
                    raise ValueError(f"Incomplete Git response for blob {oid}")
                try:
                    data.decode("utf-8")
                except UnicodeDecodeError:
                    pass
                else:
                    if b"\0" not in data:
                        filename.write_bytes(data)
                inspected.add(oid)
            if filename.is_file():
                record["source"] = f"blobs/{filename.name}"
        if "source" not in record:
            record["gap"] = "Not a regular UTF-8 text blob of at most 1 MiB."
        index.write(json.dumps(record) + "\n")


def prepare(base, head, output):
    output.mkdir()
    blobs = output / "blobs"
    blobs.mkdir()
    inspected = set()
    with subprocess.Popen(["git", "--no-replace-objects", "cat-file", "--batch"],
                          stdin=subprocess.PIPE, stdout=subprocess.PIPE) as batch:
        try:
            for name, revision in (("base", base), ("head", head)):
                with (output / f"{name}.jsonl").open("w", encoding="utf-8") as index:
                    export_revision(revision, index, blobs, batch, inspected)
        finally:
            batch.stdin.close()
        if batch.wait() != 0:
            raise RuntimeError("Git blob export failed")
    diff = ("diff", "--no-ext-diff", "--no-textconv", "--no-color", "--no-renames",
            "--ignore-submodules=none")
    for name, flags in (("files.txt", ("--name-status",)), ("diff.txt", ())):
        with (output / name).open("wb") as stream:
            subprocess.run(["git", "--no-replace-objects", *diff, *flags, base, head, "--"],
                           stdout=stream, check=True)


if __name__ == "__main__":
    prepare(sys.argv[1], sys.argv[2], Path(sys.argv[3]))
