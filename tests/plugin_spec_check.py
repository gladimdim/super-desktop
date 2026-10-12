#!/usr/bin/env python3
"""Checks the plugin skills against themselves and the fixture corpus.

- Every manifest in tests/fixtures/plugins/manifests/valid passes
  manifest.schema.json and every one in invalid/ fails it. The Rust validator
  is checked against the same corpus (`cargo test plugin_spec_`), so the two
  must agree.
- The example manifests and the SKILL.md sample validate.
- Every $ref in host-api.openrpc.json resolves, method names are unique, every
  x-permission is a known permission, and references/host-api.md mentions
  every method.
- Each example's sd_plugin.py is identical to sdk/python/sd_plugin.py.

Needs the `jsonschema` Python package; without it the schema checks are
skipped (reported), the others still run.
"""
import json
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
SKILL = ROOT / "skills/super-desktop-plugin"
CORPUS = ROOT / "tests/fixtures/plugins/manifests"
failures = []


def fail(message):
    failures.append(message)
    print("FAIL", message)


def load(path):
    return json.loads(path.read_text())


manifest_schema = load(SKILL / "schemas/manifest.schema.json")
ui_schema = load(SKILL / "schemas/ui.schema.json")
rpc = load(SKILL / "schemas/host-api.openrpc.json")

try:
    from jsonschema import Draft202012Validator
except ImportError:
    Draft202012Validator = None
    print("SKIP schema checks: python package `jsonschema` is not installed")

if Draft202012Validator:
    Draft202012Validator.check_schema(manifest_schema)
    Draft202012Validator.check_schema(ui_schema)
    validator = Draft202012Validator(manifest_schema)
    for path in sorted((CORPUS / "valid").glob("*.json")):
        errors = [f"{list(e.path)}: {e.message}" for e in validator.iter_errors(load(path))]
        if errors:
            fail(f"valid/{path.name} rejected by the schema: {errors}")
    for path in sorted((CORPUS / "invalid").glob("*.json")):
        if not any(True for _ in validator.iter_errors(load(path))):
            fail(f"invalid/{path.name} accepted by the schema")
    for path in sorted(SKILL.glob("examples/*/super-desktop-plugin.json")):
        if any(True for _ in validator.iter_errors(load(path))):
            fail(f"{path.relative_to(ROOT)} rejected by the schema")
    sample = re.search(r"`super-desktop-plugin.json`:\n\n```json\n(.*?)```", (SKILL / "SKILL.md").read_text(), re.S)
    if not sample or any(True for _ in validator.iter_errors(json.loads(sample.group(1)))):
        fail("the SKILL.md sample manifest is missing or invalid")

names = [m["name"] for m in rpc["methods"]]
if len(names) != len(set(names)):
    fail("duplicate OpenRPC method names")


def refs(node):
    if isinstance(node, dict):
        for key, value in node.items():
            if key == "$ref":
                yield value
            else:
                yield from refs(value)
    elif isinstance(node, list):
        for value in node:
            yield from refs(value)


for ref in refs(rpc):
    if ref.startswith("#/components/schemas/"):
        if ref.rsplit("/", 1)[1] not in rpc["components"]["schemas"]:
            fail(f"unresolved {ref}")
    elif ref.startswith("ui.schema.json#/$defs/"):
        if ref.rsplit("/", 1)[1] not in ui_schema["$defs"]:
            fail(f"unresolved {ref}")
    else:
        fail(f"unexpected $ref {ref}")

permissions = set(manifest_schema["$defs"]["permission"]["enum"])
for method in rpc["methods"]:
    permission = method.get("x-permission")
    if permission and permission not in permissions:
        fail(f"{method['name']} needs unknown permission {permission}")

host_api = (SKILL / "references/host-api.md").read_text()
for name in names:
    if name not in host_api:
        fail(f"references/host-api.md does not mention {name}")

sdk = (SKILL / "sdk/python/sd_plugin.py").read_bytes()
for copy in SKILL.glob("examples/*/sd_plugin.py"):
    if copy.read_bytes() != sdk:
        fail(f"{copy.relative_to(ROOT)} differs from sdk/python/sd_plugin.py")

print(f"{len(failures)} failure(s)")
sys.exit(1 if failures else 0)
