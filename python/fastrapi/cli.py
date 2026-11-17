import argparse
import importlib
import sys
from pathlib import Path

from .fastrapi import FastrAPI, __version__


DEFAULT_PATHS = ("main.py", "app.py", "api.py", "app/main.py", "app/app.py", "app/api.py")


def load_app(target, name=None):
    module_name, attribute = target, name
    if ":" in target and not Path(target).exists():
        module_name, attribute = target.rsplit(":", 1)
    file = Path(module_name)
    if file.suffix == ".py" or file.is_file():
        sys.path.insert(0, str(file.resolve().parent))
        module_name = file.stem
    module = importlib.import_module(module_name)
    if attribute:
        return getattr(module, attribute)
    candidates = [getattr(module, "app", None), getattr(module, "api", None), *vars(module).values()]
    app = next((value for value in candidates if isinstance(value, FastrAPI)), None)
    if app is None:
        raise SystemExit(f"Could not find a FastrAPI app in {target}")
    return app


def main():
    parser = argparse.ArgumentParser(prog="fastrapi")
    parser.add_argument("--version", action="version", version=f"fastrapi {__version__}")
    commands = parser.add_subparsers(dest="command", required=True)
    for command, host, reload in (("run", "0.0.0.0", False), ("dev", "127.0.0.1", True)):
        sub = commands.add_parser(command)
        sub.add_argument("path", nargs="?")
        sub.add_argument("--app")
        sub.add_argument("--host", default=host)
        sub.add_argument("--port", type=int, default=8000)
        sub.add_argument("--reload-dir", action="append")
        sub.add_argument("--reload", action=argparse.BooleanOptionalAction, default=reload)
    args = parser.parse_args()
    path = args.path or next((path for path in DEFAULT_PATHS if Path(path).is_file()), None)
    if path is None:
        raise SystemExit("Could not find a default file to run, please provide an explicit path")
    app = load_app(path, args.app)
    reload_dirs = (args.reload_dir or [str(Path.cwd())]) if args.reload else None
    app.serve(args.host, args.port, reload=args.reload, reload_dirs=reload_dirs)
