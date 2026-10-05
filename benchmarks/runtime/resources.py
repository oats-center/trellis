"""Sample owned benchmark descendants, not unrelated processes or host totals."""

import json
import os
from pathlib import Path
import sys
import time

root_pid = int(sys.argv[1])
output = Path(sys.argv[2])
ticks = os.sysconf("SC_CLK_TCK")

with (output / "resources.jsonl").open("w") as stream:
    while not (output / "stop-sampler").exists():
        processes = {}
        for directory in Path("/proc").iterdir():
            if not directory.name.isdigit():
                continue
            try:
                stat = (directory / "stat").read_text().rsplit(") ", 1)[1].split()
                processes[int(directory.name)] = (directory, stat)
            except (FileNotFoundError, ProcessLookupError, PermissionError):
                continue
        owned = {root_pid}
        while True:
            children = {pid for pid, (_, stat) in processes.items()
                        if int(stat[1]) in owned}
            expanded = owned | children
            if expanded == owned:
                break
            owned = expanded
        try:
            phase = (output / "phase.txt").read_text().strip()
        except FileNotFoundError:
            phase = "setup"
        rows = []
        for pid in sorted(owned):
            if pid not in processes or pid == os.getpid():
                continue
            proc, stat = processes[pid]
            try:
                command = (proc / "cmdline").read_bytes().replace(b"\0", b" ").decode()
                if pid == root_pid:
                    role = "coordinator"
                elif "trellis-server" in command:
                    role = "trellis-server"
                elif "nats-server" in command:
                    role = "nats"
                elif "benchmarks/runtime/http.ts" in command:
                    role = "http-provider"
                elif "worker.ts" in command:
                    role = "provider" if "--provider" in command else "load-generator"
                elif "trellis-performance" in command:
                    role = "provider" if "--provider" in command else "http-provider" if "--http-provider" in command else "load-generator"
                else:
                    role = "coordinator" if pid == root_pid else "setup-child"
                memory = {}
                for line in (proc / "smaps_rollup").read_text().splitlines():
                    if line.startswith(("Rss:", "Pss:", "Private_Clean:", "Private_Dirty:")):
                        key, value, *_ = line.split()
                        memory[key[:-1] + "KiB"] = int(value)
                rows.append({
                    "pid": pid, "startTicks": int(stat[19]), "role": role,
                    "userCpuSeconds": int(stat[11]) / ticks,
                    "systemCpuSeconds": int(stat[12]) / ticks,
                    "threads": int(stat[17]), "fds": len(list((proc / "fd").iterdir())),
                    **memory,
                })
            except (FileNotFoundError, ProcessLookupError, PermissionError):
                continue
        stream.write(json.dumps({"unixMs": time.time() * 1000, "phase": phase,
                                 "processes": rows}) + "\n")
        stream.flush()
        time.sleep(0.25)
