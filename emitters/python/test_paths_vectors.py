"""Check a Python vascular_paths against contract/paths_vectors.json.

Run: python3 emitters/python/test_paths_vectors.py <path/to/vascular_paths.py>
Umbrella CI runs it against the vendored copy; pulse's Rust port reads the same file.
"""
import importlib.util
import json
import os
import sys
from pathlib import Path

VECTORS = json.loads((Path(__file__).parents[2] / "contract" / "paths_vectors.json").read_text())


def main(module_path: str) -> int:
    spec = importlib.util.spec_from_file_location("vascular_paths", module_path)
    vp = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(vp)
    failed = 0
    saved = dict(os.environ)
    for case in VECTORS["cases"]:
        os.environ.clear()
        os.environ.update(case["env"])
        try:
            got = str(getattr(vp, case["call"])(*case["args"]))
        except Exception:
            got = {"error": True}
        if got != case["expect"]:
            failed += 1
            print(f"FAIL {case['call']}{tuple(case['args'])} env={case['env']}: got {got!r}, want {case['expect']!r}")
    os.environ.clear()
    os.environ.update(saved)
    print(f"{len(VECTORS['cases']) - failed}/{len(VECTORS['cases'])} cases pass")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1]))
