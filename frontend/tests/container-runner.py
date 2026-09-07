#!/usr/bin/env python3
"""Run the browser fixture against a release image on isolated port 8081."""
import argparse
import os
from pathlib import Path
import re
import signal
import subprocess

parser = argparse.ArgumentParser()
parser.add_argument("--config", required=True)
args = parser.parse_args()
config = Path(args.config).resolve()
directory = config.parent
text = config.read_text()
text = re.sub(r'listen\s*=\s*"127.0.0.1:8081"', 'listen="0.0.0.0:8080"', text)
text = text.replace("http://127.0.0.1:", "http://host.docker.internal:")
config.write_text(text)
name = "quotamux-workspace-e2e"

def stop(*_):
    subprocess.run(["docker", "stop", "-t", "2", name], capture_output=True)

signal.signal(signal.SIGTERM, stop)
signal.signal(signal.SIGINT, stop)
command = [
    "docker", "run", "--rm", "--name", name,
    "--read-only", "--cap-drop", "ALL", "--security-opt", "no-new-privileges:true",
    "-p", "127.0.0.1:8081:8080", "-v", f"{directory}:{directory}",
    os.environ.get("QUOTAMUX_E2E_IMAGE", "quotamux:routing-0.2.0-candidate"),
    "--config", str(config),
]
try:
    result = subprocess.run(command)
    raise SystemExit(result.returncode)
finally:
    subprocess.run(["docker", "rm", "-f", name], capture_output=True)
