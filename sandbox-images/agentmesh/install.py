# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Install the validated upstream server owner and native ACS at image build time."""

from pathlib import Path
import os
import platform
import runpy
import subprocess
import sys


def main():
    select = runpy.run_path("/tmp/install-agt-wheels.py")["select_wheels"]
    wheels = select(Path("/tmp/agt-wheels"), platform.machine())
    core = next(wheel for wheel in wheels if wheel.name.startswith("agent_governance_toolkit_core-"))
    native = next(wheel for wheel in wheels if wheel.name.startswith("agent_control_specification-"))
    subprocess.run([
        sys.executable, "-m", "pip", "install", "--no-cache-dir",
        "--target", "/opt/agentmesh", "--constraint", "/tmp/constraints.txt",
        str(core) + "[server]", str(native),
    ], check=True)
    subprocess.run([sys.executable, "-m", "pip", "check"],
                   env={**os.environ, "PYTHONPATH": "/opt/agentmesh"}, check=True)


if __name__ == "__main__":
    main()
