#!/usr/bin/env python3
"""Load a sim --crate-type lib artifact and call sim_add."""

import ctypes
import sys
from pathlib import Path


def main() -> None:
    lib_path = Path(sys.argv[1] if len(sys.argv) > 1 else "out/libadd.dylib")
    lib = ctypes.CDLL(str(lib_path.resolve()))
    lib.sim_add.argtypes = [ctypes.c_int64, ctypes.c_int64]
    lib.sim_add.restype = ctypes.c_int64
    print(lib.sim_add(40, 2))


if __name__ == "__main__":
    main()
