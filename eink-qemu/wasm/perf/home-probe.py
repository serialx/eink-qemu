#!/usr/bin/env python3
"""The benchmark CLI limited to startup -> third X3 refresh."""
import sys
from benchmark import main
if __name__ == '__main__':
    sys.argv += ['--stop-at', 'home']
    main()
