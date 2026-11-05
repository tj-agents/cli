import subprocess
import sys
import time

child = subprocess.Popen([sys.executable, sys.argv[1], sys.argv[2], sys.argv[3]])
time.sleep(float(sys.argv[5]))
sys.exit(int(sys.argv[4]))
