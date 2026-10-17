"""Print a conformance log as an aligned table: `python tests/conformance/show.py FILE...`."""

import json
import sys

for path in sys.argv[1:]:
    print(f"### {path}")
    with open(path) as f:
        for raw in f:
            d = json.loads(raw)
            print(
                f"{d['time']:>8} {d['phase']:9} {d['trigger']:44} {d['task']:10} "
                f"{d['op']:5} {d['signal']}={d['value']}"
            )
