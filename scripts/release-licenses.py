import json
import subprocess
import sys
from pathlib import Path

build, report = map(Path, sys.argv[1:])
about = json.loads(report.read_text(encoding="utf-8"))
licenses = about["licenses"]
covered = set()
for entry in licenses:
    crates = [use["crate"] for use in entry["used_by"]]
    names = ", ".join(crate["name"] for crate in crates)
    if not (entry["text"] or "").strip():
        sys.exit(f"Empty {entry['id']} license text: {names}")
    if entry["source_path"] is None and any((crate["source"] or "").startswith("git+") for crate in crates):
        sys.exit(f"Missing checkout license: {names}")
    covered.update(crate["id"] for crate in crates)
for item in about["crates"]:
    if item["package"]["id"] not in covered:
        sys.exit(f"Missing license text: {item['package']['id']}")

package = subprocess.check_output(["cargo", "pkgid", "--locked", "-p", "sumie-vt"], text=True).strip()
with build.open(encoding="utf-8") as lines:
    for message in map(json.loads, lines):
        if message["reason"] == "build-script-executed" and message["package_id"] == package:
            ghostty = Path(message["out_dir"]) / "LICENSE.ghostty"
            break
    else:
        sys.exit("No sumie-vt build output in Cargo messages")

print("Third-party licenses")
for entry in licenses:
    print(f"\n{entry['name']}")
    for use in entry["used_by"]:
        crate = use["crate"]
        print(f"- {crate['name']} {crate['version']}")
    print(entry["text"])

notices = (
    ("libghostty-vt (Ghostty)", ghostty),
    ("libghostty-rs bindings", Path("crates/sumie-vt/LICENSE.bindings")),
)
for title, path in notices:
    text = path.read_text(encoding="utf-8")
    if not text.strip():
        sys.exit(f"Empty license file: {path}")
    print(f"\n{title}\n{text}")
