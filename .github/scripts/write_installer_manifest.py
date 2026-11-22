"""Generate local Inno manifests using the same generator as release publication."""
import argparse
import datetime
import hashlib
from pathlib import Path

from winget_release import manifest_bytes, manifests, verify_pe


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--installer", type=Path, required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    verify_pe(args.installer, {0x14C, 0x8664})
    digest = hashlib.sha256(args.installer.read_bytes()).hexdigest()
    date = datetime.datetime.now(datetime.timezone.utc).isoformat()
    files = manifests(args.version, date, digest, "inno")
    args.output.mkdir(parents=True, exist_ok=True)
    expected = {Path(name).name for name in files}
    if any(path.name not in expected for path in args.output.iterdir()):
        raise ValueError("Manifest directory contains unrelated files")
    for name, content in files.items():
        (args.output / Path(name).name).write_bytes(manifest_bytes(content))


if __name__ == "__main__":
    main()
