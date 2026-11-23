#!/usr/bin/env python3
"""discord-wave17b on the Android lab host. The prove is chatd's, same as the desktop driver."""

import os
import runpy
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.argv[0] = os.path.join(HERE, "drive.py")
runpy.run_path(sys.argv[0], run_name="__main__")
