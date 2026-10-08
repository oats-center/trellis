"""Sample owned benchmark descendants, not unrelated processes or host totals."""

import json
import os
import re
from pathlib import Path
import sys
import time
import urllib.request

root_pid = int(sys.argv[1])
output = Path(sys.argv[2])
ticks = os.sysconf("SC_CLK_TCK")
monitor_urls = {}
nats_stream = (output / "nats.jsonl").open("w") if "--nats-diagnostics" in sys.argv else None

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
        tcp_ports = {}
        if nats_stream:
            for table in (Path("/proc/net/tcp"), Path("/proc/net/tcp6")):
                for line in table.read_text().splitlines()[1:]:
                    columns = line.split()
                    tcp_ports[columns[9]] = int(columns[1].split(":")[1], 16)
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
                    if nats_stream and pid not in monitor_urls:
                        arguments = (proc / "cmdline").read_bytes().decode().split("\0")
                        # Bootstrap also runs nats-server --version before the broker starts.
                        if "-c" in arguments:
                            config = Path(arguments[arguments.index("-c") + 1]).read_text()
                            monitor = re.search(r"(?m)^http:\s*127\.0\.0\.1:(\d+)\s*$", config)
                            if monitor:
                                monitor_urls[pid] = f"http://127.0.0.1:{monitor.group(1)}"
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
                if nats_stream:
                    local_ports = set()
                    for fd in (proc / "fd").iterdir():
                        try:
                            target = os.readlink(fd)
                            if target.startswith("socket:["):
                                port = tcp_ports.get(target[8:-1])
                                if port is not None:
                                    local_ports.add(port)
                        except FileNotFoundError:
                            pass
                    rows[-1]["tcpLocalPorts"] = sorted(local_ports)
            except (FileNotFoundError, ProcessLookupError, PermissionError):
                continue
        stream.write(json.dumps({"unixMs": time.time() * 1000, "phase": phase,
                                 "processes": rows}) + "\n")
        stream.flush()
        if nats_stream:
            for pid, url in monitor_urls.items():
                if pid not in owned:
                    continue
                for endpoint in ("varz", "connz?state=any&limit=1024"):
                    try:
                        with urllib.request.urlopen(f"{url}/{endpoint}", timeout=0.5) as response:
                            diagnostic = json.load(response)
                        row = {"unixMs": time.time() * 1000, "phase": phase,
                               "pid": pid, "endpoint": endpoint, "data": diagnostic}
                    except Exception as error:
                        row = {"unixMs": time.time() * 1000, "phase": phase,
                               "pid": pid, "endpoint": endpoint, "error": str(error)}
                    nats_stream.write(json.dumps(row) + "\n")
            nats_stream.flush()
        time.sleep(0.25)
if nats_stream:
    nats_stream.close()
