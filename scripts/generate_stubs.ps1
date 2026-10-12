$ErrorActionPreference = "Stop"

$RepoRoot = Split-Path -Parent $PSScriptRoot
Set-Location $RepoRoot

$PythonExe = if (Test-Path "$RepoRoot\.venv\Scripts\python.exe") {
    "$RepoRoot\.venv\Scripts\python.exe"
} else {
    "python"
}

& $PythonExe "$RepoRoot\scripts\generate_stubs.py"
