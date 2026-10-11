#!/usr/bin/env python3
"""Run the offline DSP soak under bounded CPU load and sample peak resident memory."""
import hashlib
import multiprocessing
import os
from pathlib import Path
import subprocess
import sys
import time


def burn(stop):
    block = b'BigSound CPU stress' * 256
    while not stop.is_set():
        hashlib.sha256(block).digest()


def main(binary, seconds):
    context = multiprocessing.get_context("fork")
    stop = context.Event()
    workers = [context.Process(target=burn, args=(stop,)) for _ in range(min(4, max(1, (os.cpu_count() or 2) // 4)))]
    peak_rss = 0
    proc = None
    start = time.monotonic()
    try:
        for worker in workers:
            worker.start()
        proc = subprocess.Popen([str(binary), str(seconds)])
        while proc.poll() is None:
            try:
                for line in Path(f'/proc/{proc.pid}/status').read_text().splitlines():
                    if line.startswith('VmRSS:'):
                        peak_rss = max(peak_rss, int(line.split()[1]))
            except FileNotFoundError:
                pass
            time.sleep(.1)
        if proc.returncode:
            raise SystemExit(proc.returncode)
        print(f'CPU workers: {len(workers)}; wall seconds: {time.monotonic()-start:.2f}; peak DSP process RSS KiB: {peak_rss}', file=sys.stderr)
    finally:
        if proc and proc.poll() is None:
            proc.terminate()
            proc.wait(timeout=5)
        stop.set()
        for worker in workers:
            if worker.pid:
                worker.join(timeout=3)
                if worker.is_alive():
                    worker.terminate()
                    worker.join()


if __name__ == '__main__':
    main(Path(sys.argv[1]).resolve(), int(sys.argv[2]) if len(sys.argv) > 2 else 600)
