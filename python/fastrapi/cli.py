import argparse
import importlib
import sys
from pathlib import Path

from .fastrapi import FastrAPI, __version__


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


def main(argv=None):
    parser = argparse.ArgumentParser(prog="fastrapi")
    parser.add_argument("--version", action="version", version=f"fastrapi {__version__}")
    commands = parser.add_subparsers(dest="command", required=True)
    for command, host, reload in (("run", "0.0.0.0", False), ("dev", "127.0.0.1", True)):
        sub = commands.add_parser(command)
        sub.add_argument("path", nargs="?", default="main.py")
        sub.add_argument("--app")
        sub.add_argument("--host", default=host)
        sub.add_argument("--port", type=int, default=8000)
        sub.add_argument("--reload-dir", action="append")
        sub.add_argument("--reload", action=argparse.BooleanOptionalAction, default=reload)
    args = parser.parse_args(argv)
    app = load_app(args.path, args.app)
    reload_dirs = (args.reload_dir or [str(Path.cwd())]) if args.reload else None
    app.serve(args.host, args.port, reload=args.reload, reload_dirs=reload_dirs)
