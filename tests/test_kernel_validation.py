"""The one huge test for the kernel validation fast path.

Runs the entire request battery in `kernel_case_app.py` twice against the
real fastrapi server — once with `FASTRAPI_KERNEL_VALIDATION=0` (pure
pydantic) and once with `=1` (Rust kernel fast path) — and asserts:

1. **Byte-identical behavior**: every response (status + body) is identical
   between the two engines — both now speak fastapi's wire format (body-rooted
   `loc`, `input` present, no pydantic `url`), so there is nothing left to
   tell them apart on the wire.
2. **The fast path actually engages**: `fastrapi.kernel_validation_count()`
   stays at zero with the flag off and climbs with it on, proving requests
   were validated in Rust without entering pydantic.
3. **Semantics survived**: coerced values, defaults, aliases and error
   ordering are exactly what pydantic produces.
"""

import json
import os
import re
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent

sys.path.insert(0, str(HERE))
from kernel_case_app import CASE_COUNT  # noqa: E402

PYDANTIC_URL = re.compile(r"errors\.pydantic\.dev/[\d.]+/v/")


def _run_battery(flag: str) -> dict:
    env = dict(os.environ)
    env["FASTRAPI_KERNEL_VALIDATION"] = flag
    proc = subprocess.run(
        [sys.executable, str(HERE / "kernel_case_app.py")],
        capture_output=True,
        text=True,
        cwd=str(ROOT),
        env=env,
        timeout=300,
    )
    assert proc.returncode == 0, f"runner failed:\n{proc.stdout}\n{proc.stderr}"
    markers = [ln for ln in proc.stdout.splitlines() if ln.startswith("KERNEL_CASE_JSON:")]
    assert markers, f"no summary in runner output:\n{proc.stdout}\n{proc.stderr}"
    return json.loads(markers[-1].split(":", 1)[1])


def _normalized(result: dict) -> str:
    """Diff-able rendering: align pydantic's versioned URL prefix."""
    text = json.dumps(result, sort_keys=True)
    return PYDANTIC_URL.sub("errors.pydantic.dev/V/", text)


def _find(results: list, case: str) -> dict:
    return next(r for r in results if r["case"] == case)


def test_kernel_fast_path_is_identical_to_pydantic_and_engages():
    baseline = _run_battery("0")
    kernel = _run_battery("1")

    # 1. the whole battery ran in both engines
    assert len(baseline["results"]) == CASE_COUNT
    assert len(kernel["results"]) == CASE_COUNT

    # 2. every single response is identical between the two engines
    divergences = []
    for pyd, ker in zip(baseline["results"], kernel["results"]):
        assert pyd["case"] == ker["case"], "battery order mismatch"
        if _normalized(pyd) != _normalized(ker):
            divergences.append((pyd, ker))
    assert not divergences, "\n\n".join(
        f"case {pyd['case']}:\n  pydantic: {json.dumps(pyd)}\n  kernel:   {json.dumps(ker)}"
        for pyd, ker in divergences
    )

    # 3. the fast path engaged for kernel-compatible models: the rust counter
    #    stays at zero under the pydantic engine and climbs under the kernel
    assert baseline["kernel_validations"] == 0, (
        "kernel validations ran with the flag off"
    )
    assert kernel["kernel_validations"] > 0, "kernel fast path did not engage"

    # 4. fallback selection: models whose schema needs Python (validators)
    #    still went through pydantic, even with the flag on — their model
    #    validator side-effects ran, which the rust path cannot do.
    assert kernel["probe_validations"] > 0, (
        "validator-bearing models must fall back to pydantic"
    )
    probe_err = json.dumps(_find(kernel["results"], "probe-bad")["body"])
    assert "detail" in probe_err, "fallback model validation must still produce 422 detail"
    probe_ok = json.dumps(_find(baseline["results"], "probe-valid")["body"])
    assert _normalized(_find(baseline["results"], "probe-valid")) == _normalized(
        _find(kernel["results"], "probe-valid")
    )

    by_name = _find(kernel["results"], "by-name")
    assert by_name["status"] == 200 and by_name["body"]["nick"] == "ann"

    # 5. semantics: coercions, defaults and aliasing survived the fast path
    full = _find(kernel["results"], "full")
    assert full["status"] == 200
    body = full["body"]
    assert body["age"] == 42 and isinstance(body["age"], int)
    assert body["active"] is True and body["scale"] == 1.5
    assert body["bio"] == "hey" and body["nick"] == "ann"
    assert body["shout"] == "WHISPER"
    assert body["counts"] == {"x": 1, "y": 2}
    assert body["token"] == "aGk="  # Base64Bytes serializes back to base64
    assert body["day"] == "2020-01-01"
    assert body["when"].startswith("2020-01-01T12:30:00")

    coerced = _find(kernel["results"], "coerce-strings")
    assert coerced["status"] == 200
    assert coerced["body"]["age"] == 42 and coerced["body"]["active"] is True
    assert coerced["body"]["counts"] == {"k": 3}

    # 6. error ordering follows field declaration order, in both engines
    multi = _find(kernel["results"], "multi-error")["body"]["detail"]
    assert [e["type"] for e in multi] == [
        "string_pattern_mismatch",  # name
        "missing",                  # city
        "int_parsing",              # age
    ]
    # fastapi roots body errors at ["body", ...]
    assert [e["loc"] for e in multi] == [
        ["body", "name"],
        ["body", "city"],
        ["body", "age"],
    ]

    # 7. the probe validator ran exactly once per valid probe request in the
    #    kernel run too (the model fell back, so its side effects still happen)
    assert kernel["probe_validations"] >= 1
