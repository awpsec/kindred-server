#!/usr/bin/env python3
"""Fail closed on older images that do not contain host-network support."""
import os
from pathlib import Path
import sys

if os.environ.get('KINDRED_NETWORK_MODE') != 'host':
    raise SystemExit('Linux host networking requires KINDRED_NETWORK_MODE=host')
os.execv(sys.executable, [sys.executable, str(Path(__file__).with_name('container-entry.py'))])
