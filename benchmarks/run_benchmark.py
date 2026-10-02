from __future__ import annotations

import argparse
import os
import re
import shutil
import subprocess
import sys
import time
import urllib.error
import urllib.request
import zipfile
from dataclasses import dataclass
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
BENCHMARKS = Path(__file__).resolve().parent


@dataclass(frozen=True)
class Benchmark:
    name: str
    directory: Path
    stress: Path


@dataclass
class BenchmarkRun:
    label: str
    framework: str
    benchmark: str
    req_per_sec: float
    raw_output: str


class BenchmarkError(RuntimeError):
    pass


def discover_benchmarks() -> list[Benchmark]:
    found = []
    for directory in sorted(BENCHMARKS.iterdir()):
        if not directory.is_dir() or directory.name.startswith("__"):
            continue
        name = directory.name
        expected = [directory / f"fastapi_{name}.py", directory / f"fastrapi_{name}.py", directory / "stress.js"]
        if all(path.is_file() for path in expected):
            found.append(Benchmark(name, directory, expected[2]))
    return found


def require_tool(name: str) -> None:
    if shutil.which(name) is None:
        raise BenchmarkError(f"Required tool not found: {name}")


def run_command(command: list[str], cwd: Path) -> None:
    completed = subprocess.run(command, cwd=cwd, text=True)
    if completed.returncode:
        raise BenchmarkError(f"Command failed ({completed.returncode}): {' '.join(command)}")


def build_wheel(source_root: Path, out_dir: Path, profile: str) -> Path:
    out_dir.mkdir(parents=True, exist_ok=True)
    command = ["maturin", "build", "-i", sys.executable, "-o", str(out_dir)]
    if profile == "release":
        command.insert(2, "--release")
    run_command(command, source_root)
    wheels = sorted(out_dir.glob("*.whl"), key=lambda path: path.stat().st_mtime)
    if not wheels:
        raise BenchmarkError(f"No wheel produced in {out_dir}")
    return wheels[-1]


def export_ref(ref: str, destination: Path) -> Path:
    archive_path = destination.parent / "source.zip"
    run_command(["git", "archive", "--format=zip", ref, "-o", str(archive_path)], ROOT)
    with zipfile.ZipFile(archive_path) as archive:
        archive.extractall(destination)
    return destination


def extract_wheel(wheel_path: Path, destination: Path) -> None:
    destination.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(wheel_path) as archive:
        archive.extractall(destination)


def wait_for_server(url: str, process: subprocess.Popen[str], timeout_seconds: float) -> None:
    deadline = time.time() + timeout_seconds
    while time.time() < deadline:
        if process.poll() is not None:
            stdout, stderr = process.communicate()
            raise BenchmarkError(f"Server exited before it was ready.\nstdout:\n{stdout}\nstderr:\n{stderr}")
        try:
            with urllib.request.urlopen(url, timeout=1) as response:
                if response.status in (200, 405):
                    return
        except urllib.error.HTTPError as error:
            if error.code == 405:
                return
        except (urllib.error.URLError, TimeoutError):
            pass
        time.sleep(0.25)
    process.terminate()
    stdout, stderr = process.communicate()
    raise BenchmarkError(f"Server did not start in time.\nstdout:\n{stdout}\nstderr:\n{stderr}")


def run_k6(script_path: Path, base_url: str) -> tuple[float, str]:
    completed = subprocess.run(
        ["k6", "run", str(script_path)],
        cwd=ROOT,
        env={**os.environ, "BASE_URL": base_url},
        text=True,
        capture_output=True,
    )
    output = (completed.stdout or "") + (completed.stderr or "")
    if completed.returncode:
        raise BenchmarkError(output.strip())
    match = re.search(r"http_reqs[^\n]*?([0-9]+(?:\.[0-9]+)?)/s", output)
    if match is None:
        raise BenchmarkError("Could not parse req/s from k6 output.")
    return float(match.group(1)), output


def run_benchmark(
    source_root: Path,
    benchmark: Benchmark,
    framework: str,
    host: str,
    port: int,
    startup_timeout: float,
    label: str,
    extract_dir: Path | None,
) -> BenchmarkRun:
    if framework == "fastrapi" and extract_dir is None:
        raise BenchmarkError("fastrapi benchmarks require a built wheel")
    app_path = benchmark.directory / f"{framework}_{benchmark.name}.py"
    code = f"import runpy;runpy.run_path({str(app_path)!r}, run_name='__main__')"
    import_env = {"BENCHMARK_HOST": host, "BENCHMARK_PORT": str(port)}
    if extract_dir is not None:
        code = f"import sys;sys.path.insert(0,{str(extract_dir)!r});" + code
    process = subprocess.Popen(
        [sys.executable, "-c", code],
        cwd=source_root,
        env={**os.environ, **import_env},
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    try:
        base_url = f"http://{host}:{port}"
        wait_for_server(f"{base_url}/", process, startup_timeout)
        req_per_sec, raw_output = run_k6(benchmark.stress, base_url)
        return BenchmarkRun(label, framework, benchmark.name, req_per_sec, raw_output)
    finally:
        if process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
        process.communicate()


def print_result(result: BenchmarkRun) -> None:
    print(f"[{result.label}/{result.framework}/{result.benchmark}] requests/sec: {result.req_per_sec:.3f}", flush=True)


def prepare_source(source_root: Path, work_root: Path, profile: str, label: str) -> tuple[Path, Path]:
    build_dir = work_root / f"{label}_build"
    extract_dir = work_root / f"{label}_wheel"
    wheel = build_wheel(source_root, build_dir, profile)
    extract_wheel(wheel, extract_dir)
    return wheel, extract_dir


def main() -> int:
    parser = argparse.ArgumentParser(description="Run discovered FastAPI/FastrAPI benchmarks.")
    parser.add_argument("--benchmark", action="append", help="Benchmark name; repeat or omit for all")
    parser.add_argument("--framework", choices=["fastapi", "fastrapi", "both"], default="both")
    parser.add_argument("--profile", choices=["dev", "release"], default="release")
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=8000)
    parser.add_argument("--compare-ref")
    parser.add_argument("--startup-timeout", type=float, default=20.0)
    parser.add_argument("--work-dir", default="benchwork")
    args = parser.parse_args()

    for tool in ("git", "k6", "maturin"):
        require_tool(tool)
    benchmarks = discover_benchmarks()
    selected = [benchmark for benchmark in benchmarks if not args.benchmark or benchmark.name in args.benchmark]
    missing = set(args.benchmark or ()) - {benchmark.name for benchmark in benchmarks}
    if missing:
        raise BenchmarkError(f"Unknown benchmark(s): {', '.join(sorted(missing))}")
    if not selected:
        raise BenchmarkError("No benchmark directories found")

    work_root = (ROOT / args.work_dir).resolve()
    if work_root.exists():
        shutil.rmtree(work_root, ignore_errors=True)
    work_root.mkdir(parents=True)
    frameworks = ["fastapi", "fastrapi"] if args.framework == "both" else [args.framework]
    try:
        _, current_extract = prepare_source(ROOT, work_root, args.profile, "current")
        current_results = {}
        for benchmark in selected:
            for framework in frameworks:
                result = run_benchmark(ROOT, benchmark, framework, args.host, args.port, args.startup_timeout, "current", current_extract if framework == "fastrapi" else None)
                current_results[(framework, benchmark.name)] = result
                print_result(result)
        if args.compare_ref:
            ref_root = export_ref(args.compare_ref, work_root / "ref_source")
            _, ref_extract = prepare_source(ref_root, work_root, args.profile, "reference")
            for benchmark in selected:
                for framework in frameworks:
                    result = run_benchmark(ref_root, benchmark, framework, args.host, args.port, args.startup_timeout, args.compare_ref, ref_extract if framework == "fastrapi" else None)
                    print_result(result)
                    current = current_results[(framework, benchmark.name)]
                    delta = current.req_per_sec - result.req_per_sec
                    pct = delta / result.req_per_sec * 100 if result.req_per_sec else 0.0
                    print(f"[delta/{framework}/{benchmark.name}] current - {args.compare_ref}: {delta:.3f} req/s ({pct:+.2f}%)")
    finally:
        shutil.rmtree(work_root, ignore_errors=True)
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except BenchmarkError as error:
        print(error, file=sys.stderr)
        raise SystemExit(1)
