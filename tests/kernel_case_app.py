"""One deep test case for the Rust kernel validation fast path.

Run directly as a script (used by `test_kernel_validation.py` in two
subprocesses — kernel flag off and on) — it prints a JSON summary of every
(status, body) pair so the parent test can diff the two engines:

    FASTRAPI_KERNEL_VALIDATION=0 python tests/kernel_case_app.py
    FASTRAPI_KERNEL_VALIDATION=1 python tests/kernel_case_app.py

The model battery is deliberately exhaustive for the kernel's coverage:
constraints, coercions, aliases, defaults, optionals, bytes, dates,
lists, dicts, literals, forbid, multi-error ordering, plus fallback
routes (nested models, validator-bearing models) that must keep going
through pydantic.
"""

import json
import re
import socket
import threading
import time
from datetime import date, datetime
from typing import Annotated, Dict, List, Literal, Optional

import httpx
from pydantic import BaseModel, ConfigDict, Field, StringConstraints, model_validator

import fastrapi
from fastrapi import FastrAPI

# ---------------------------------------------------------------------------
# models
# ---------------------------------------------------------------------------


class KitchenSink(BaseModel):
    """A flat model covering every kernel-supported validator.

    Deliberately data-only: no validators, no default_factory, no Enum —
    those contain non-JSON leaves in the core schema and must (and do) fall
    back to the Python path.
    """

    model_config = ConfigDict(extra="forbid", populate_by_name=True)

    name: str = Field(pattern=r"^[a-z]+$", min_length=2, max_length=10)
    city: str
    age: int = Field(ge=0, le=150)
    scale: float
    active: bool
    color: Literal["red", "green", "blue"]
    shout: Optional[Annotated[str, StringConstraints(to_upper=True)]] = None
    nick: Optional[str] = Field(default=None, alias="userName")
    bio: str = "hello"
    tags: List[str] = []
    counts: Dict[str, int] = {}
    token: Optional[bytes] = None
    score: Optional[int] = None
    day: Optional[date] = None
    when: Optional[datetime] = None


class Inner(BaseModel):
    qty: int


class Outer(BaseModel):
    """Nested models are kernel-unsupported (by design) → Python fallback."""

    inner: Inner
    label: str


PROBE_STATE = {"ran": 0}


class Probe(BaseModel):
    """A model_validator is a non-JSON schema leaf → Python fallback."""

    name: str

    @model_validator(mode="after")
    def mark(self):
        PROBE_STATE["ran"] += 1
        return self


app = FastrAPI(debug=True)


@app.post("/sink")
def sink(body: KitchenSink):
    return body


@app.post("/outer")
def outer(body: Outer):
    return body


@app.post("/probe")
def probe(body: Probe):
    return body


# ---------------------------------------------------------------------------
# request battery
# ---------------------------------------------------------------------------


def _body(payload) -> bytes:
    return json.dumps(payload).encode("utf-8")


def _sink(**overrides) -> dict:
    base = {
        "name": "anne",
        "city": "berlin",
        "age": 42,
        "scale": 1.5,
        "active": True,
        "color": "red",
    }
    base.update(overrides)
    return base


BATTERY = [
    # -- valid payloads (flag-on run must take the model_construct fast path) --
    ("full", "/sink", _body(_sink(
        shout="whisper", userName="ann", bio="hey", tags=["a", "b"],
        counts={"x": 1, "y": 2}, token="aGk=", score=99,
        day="2020-01-01", when="2020-01-01T12:30:00Z",
    ))),
    ("minimal", "/sink", _body(_sink())),
    ("coerce-strings", "/sink", _body(_sink(
        age="42", scale="1.5", active="true", color="blue",
        score="7", counts={"k": "3"}, when="2020-01-01T00:00:00",
    ))),
    ("alias", "/sink", _body(_sink(userName="ann"))),
    ("alias-and-name", "/sink", _body(_sink(userName="a", nick="b"))),
    ("by-name", "/sink", _body(_sink(nick="ann"))),
    ("strip-and-upper", "/sink", _body(_sink(shout="loud"))),
    ("empty-collections", "/sink", _body(_sink(tags=[], counts={}))),
    # -- single-error payloads --
    ("bad-pattern", "/sink", _body(_sink(name="Anne"))),
    ("bad-short", "/sink", _body(_sink(name="a"))),
    ("bad-long", "/sink", _body(_sink(name="a" * 11))),
    ("bad-age-neg", "/sink", _body(_sink(age=-1))),
    ("bad-age-str", "/sink", _body(_sink(age="abc"))),
    ("bad-age-float", "/sink", _body(_sink(age=1.5))),
    ("bad-bool", "/sink", _body(_sink(active="maybe"))),
    ("bad-literal", "/sink", _body(_sink(color="purple"))),
    ("bad-score", "/sink", _body(_sink(score="xyz"))),
    ("bad-date", "/sink", _body(_sink(day="not-a-date"))),
    ("bad-datetime", "/sink", _body(_sink(when="tomorrow"))),
    ("bad-b64", "/sink", _body(_sink(token="not!b64"))),
    ("bad-list-item", "/sink", _body(_sink(tags=[1]))),
    ("bad-dict-value", "/sink", _body(_sink(counts={"a": "z"}))),
    ("missing-city", "/sink", _body({"name": "anne", "age": 1, "scale": 1.0, "active": True, "color": "red"})),
    ("extra-key", "/sink", _body(_sink(sneaky=1))),
    ("wrong-type-list", "/sink", _body(_sink(tags="not-a-list"))),
    ("scale-null", "/sink", _body(_sink(scale=None))),
    ("empty-object", "/sink", _body({})),
    # -- multi-error: error order must follow field declaration order --
    ("multi-error", "/sink", _body({"name": "Anne", "age": "abc", "scale": 1.0, "active": True, "color": "red"})),
    # -- raw payloads --
    ("bad-json", "/sink", b"{oops"),
    ("bad-utf8", "/sink", b"\xff\xfe{}"),
    # -- fallback routes (kernel must decline these and let pydantic handle) --
    ("outer-valid", "/outer", _body({"inner": {"qty": 2}, "label": "x"})),
    ("outer-inner-bad", "/outer", _body({"inner": {"qty": "abc"}, "label": "x"})),
    ("probe-valid", "/probe", _body({"name": "n"})),
    ("probe-bad", "/probe", _body({"nope": 1})),
]

CASE_COUNT = len(BATTERY)


class LiveClient(httpx.Client):
    def __init__(self, app):
        self.app = app
        with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
            s.bind(("", 0))
            self.port = s.getsockname()[1]
        self.started = False
        super().__init__(base_url=f"http://127.0.0.1:{self.port}")

    def _ensure(self):
        if not self.started:
            threading.Thread(
                target=self.app.serve, args=("127.0.0.1", self.port), daemon=True
            ).start()
            time.sleep(0.8)
            self.started = True

    def post_raw(self, path: str, content: bytes):
        self._ensure()
        return self.post(path, content=content, headers={"Content-Type": "application/json"})


# Error-message skew between the *installed* pydantic-core's jiter/speedate
# versions and the ones this repo's pydantic-core (which the kernel mirrors)
# uses. Only the human text inside *_parsing / json_invalid errors differs;
# types, locs, inputs and everything else are compared strictly.
SKEWY_TYPES = {
    "json_invalid", "date_parsing", "date_from_datetime_parsing",
    "time_parsing", "datetime_parsing", "datetime_from_date_parsing",
    "timedelta_parsing",
}


def _normalize_skew(body):
    if not isinstance(body, dict) or not isinstance(body.get("detail"), list):
        return body
    for detail in body["detail"]:
        if detail.get("type") in SKEWY_TYPES:
            if isinstance(detail.get("ctx"), dict) and "error" in detail["ctx"]:
                detail["ctx"]["error"] = "<skew>"
            if isinstance(detail.get("msg"), str):
                sep = ": " if detail["type"] == "json_invalid" else ", "
                head = detail["msg"].split(sep, 1)[0]
                detail["msg"] = f"{head}{sep}<skew>"
    return body


def run_battery() -> dict:
    results = []
    with LiveClient(app) as client:
        for name, path, content in BATTERY:
            resp = client.post_raw(path, content)
            try:
                body = resp.json()
            except Exception:
                body = resp.text
            results.append(
                {"case": name, "status": resp.status_code, "body": _normalize_skew(body)}
            )
    return {
        "results": results,
        "probe_validations": PROBE_STATE["ran"],
        "kernel_validations": fastrapi.kernel_validation_count(),
    }


if __name__ == "__main__":
    summary = run_battery()
    print("KERNEL_CASE_JSON:" + json.dumps(summary))
