"""Runs a Fusion 360 script inside Fission.

Fission starts:  python -u fission_bridge.py <script.py>   (with this folder on PYTHONPATH)
and answers our requests on stdin. Each request is one JSON line on the real stdout:
    {"id": n, "cmd": "<fission command>", "params": {...}}   ->   {"id": n, "ok": true, "result": ...}
plus {"op": "message", "text": ...} for a message box, {"op": "error", "text": ...} when the script
failed, and {"op": "done"} at the end. The script's own print() goes to stderr (Fission's log).
"""
import json
import os
import runpy
import sys
import traceback

_out = sys.stdout
sys.stdout = sys.stderr
_n = 0
# Run as the program, this module is __main__: `import fission_bridge` (in adsk) must get this one,
# with the real stdout, not a second copy.
if __name__ == "__main__":
    sys.modules["fission_bridge"] = sys.modules["__main__"]


class FissionError(Exception):
    """A Fission command refused what the script asked for."""


_trace = os.environ.get("FISSION_TRACE")


def call(cmd, params=None):
    global _n
    _n += 1
    if _trace:
        import time
        with open(_trace, "a", encoding="utf-8") as fh:
            fh.write("{:.3f} {} {}\n".format(time.time(), cmd, json.dumps(params or {})[:300]))
    _out.write(json.dumps({"id": _n, "cmd": cmd, "params": params or {}}) + "\n")
    _out.flush()
    line = sys.stdin.readline()
    if not line:
        raise SystemExit("Fission stopped answering")
    r = json.loads(line)
    if not r.get("ok"):
        raise FissionError("{}: {}".format(cmd, r.get("error", "failed")))
    return r.get("result")


def message(text):
    _out.write(json.dumps({"op": "message", "text": str(text)}) + "\n")
    _out.flush()


def _send(op, **kw):
    kw["op"] = op
    _out.write(json.dumps(kw) + "\n")
    _out.flush()


def main(path):
    sys.path.insert(0, os.path.dirname(os.path.abspath(path)))
    try:
        ns = runpy.run_path(path, run_name="__fission_script__")
        run = ns.get("run")
        if callable(run):
            run({})
        _send("done")
    except SystemExit:
        _send("done")
    except Exception:
        tb = traceback.format_exc()
        # point at the script's own line
        line = None
        for f in traceback.extract_tb(sys.exc_info()[2]):
            if os.path.abspath(f.filename) == os.path.abspath(path):
                line = f.lineno
        where = " (line {})".format(line) if line else ""
        _send("error", text="The script failed{}: {}".format(where, tb.strip().splitlines()[-1]))


if __name__ == "__main__":
    if len(sys.argv) < 2:
        _send("error", text="no script given")
    else:
        main(sys.argv[1])
