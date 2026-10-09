"""Sequential Eta profiles using prebuilt public clients and disk-backed workspaces."""
import json
import hashlib
import os
import pathlib
import subprocess
import sys

root = pathlib.Path(sys.argv[1]).resolve()
source = root / "source"
results = root / "results" / sys.argv[2]
results.mkdir(parents=True, exist_ok=False)
paths = ["ts/packages/trellis/auth/authorization/verification_worker.mjs",
         "ts/packages/trellis/auth/authorization/verification_workers.ts",
         "ts/packages/trellis/auth/authorization/provider_cache.ts",
         "ts/packages/trellis/auth/protocol_wasm/trellis_protocol_wasm_bg.wasm",
         "benchmarks/runtime/run.ts", "benchmarks/runtime/profile.ts"]
(results / "source-manifest.json").write_text(json.dumps({
    name: hashlib.sha256((source / name).read_bytes()).hexdigest() for name in paths}, indent=2))
env = os.environ.copy()
env.update({"TMPDIR": str(root / "tmp"), "TMP": str(root / "tmp"), "TEMP": str(root / "tmp"),
            "DENO_DIR": str(root / "deno-cache"), "TRELLIS_CACHE_DIR": str(root / "cache")})
executions = []
for rate in [int(value) for value in sys.argv[3:]] or [200, 400]:
    for max_workers in [1, 3]:
        label = f"workers-max-{max_workers}-{rate}"
        output = results / label
        command = ["deno", "run", "-A", "-c", "ts/deno.json", "benchmarks/runtime/run.ts",
                   "--lane", "admission", "--client-language", "rust", "--provider-language", "typescript",
                   "--server", str(root / "target/release/trellis-server"), "--cli", str(root / "target/release/trellis"),
                   "--rust-bin", str(root / "target/release/trellis-performance"), "--request-limit", "1024",
                   "--request-byte-limit", str(512 * 1024**2), "--rpc-delay-ms", "500", "--rpc-value-bytes", "262144",
                   "--arrival-rate", str(rate), "--calls", str(rate * 20), "--max-outstanding", "1024", "--samples", "1",
                   "--warmups", "0", "--sessions", "1", "--sizes", "64", "--idle-seconds", "0.1",
                   "--output", str(output), "--nats-diagnostics", "--inspect-provider", "--cpu-profile-provider"]
        command.extend(["--max-verification-workers", str(max_workers)])
        print("START", label, flush=True)
        with (results / f"{label}.cpu.log").open("w") as cpu_log, (results / f"{label}.runner.log").open("w") as log:
            cpu = subprocess.Popen(["deno", "run", "-A", "-c", "ts/deno.json", "benchmarks/runtime/profile.ts",
                                    str(output), "9230", "provider", "15"], cwd=source, env=env,
                                   stdout=cpu_log, stderr=subprocess.STDOUT)
            try:
                run = subprocess.run(command, cwd=source, env=env, stdout=log, stderr=subprocess.STDOUT, timeout=300)
                cpu_exit = cpu.wait(timeout=130)
            finally:
                if cpu.poll() is None:
                    cpu.terminate()
                    cpu.wait(timeout=10)
        executions.append({"case": label, "runnerExit": run.returncode, "cpuExit": cpu_exit})
        (results / "executions.json").write_text(json.dumps(executions, indent=2))
        print("END", label, run.returncode, cpu_exit, flush=True)
