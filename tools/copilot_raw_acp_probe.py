import argparse
import json
import queue
import subprocess
import sys
import threading
import time
from pathlib import Path


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--copilot-path", required=True)
    parser.add_argument("--cwd", default=str(Path.cwd()))
    parser.add_argument("--timeout", type=float, default=30.0)
    args = parser.parse_args()

    start = time.perf_counter()
    proc = subprocess.Popen(
        [args.copilot_path, "--acp", "--stdio"],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        encoding="utf-8",
        errors="replace",
        bufsize=1,
    )

    events = queue.Queue()
    first_stdout_time = {"value": None}

    def elapsed() -> float:
        return time.perf_counter() - start

    def read_stdout() -> None:
        assert proc.stdout is not None
        for line in proc.stdout:
            now = elapsed()
            if first_stdout_time["value"] is None:
                first_stdout_time["value"] = now
            events.put(("stdout", now, line.rstrip("\r\n")))
        events.put(("stdout_closed", elapsed(), None))

    def read_stderr() -> None:
        assert proc.stderr is not None
        for line in proc.stderr:
            events.put(("stderr", elapsed(), line.rstrip("\r\n")))
        events.put(("stderr_closed", elapsed(), None))

    threading.Thread(target=read_stdout, daemon=True).start()
    threading.Thread(target=read_stderr, daemon=True).start()

    print(f"spawned pid={proc.pid} t+{elapsed():.3f}s")

    initialize_request = {
        "jsonrpc": "2.0",
        "id": 0,
        "method": "initialize",
        "params": {
            "protocolVersion": 1,
            "clientCapabilities": {
                "terminal": True,
            },
            "clientInfo": {
                "name": "raw-acp-probe",
                "title": "raw-acp-probe",
                "version": "0.1.0",
            },
        },
    }
    send_json(proc, initialize_request)
    print(f"sent initialize t+{elapsed():.3f}s")
    init_response = wait_for_response(events, 0, args.timeout)
    print(
        f"first stdout t+{first_stdout_time['value']:.3f}s"
        if first_stdout_time["value"] is not None
        else "first stdout not observed"
    )
    print(f"initialize response t+{init_response['elapsed']:.3f}s")

    new_session_request = {
        "jsonrpc": "2.0",
        "id": 1,
        "method": "session/new",
        "params": {
            "cwd": args.cwd,
            "mcpServers": [],
        },
    }
    send_json(proc, new_session_request)
    print(f"sent session/new t+{elapsed():.3f}s")
    session_response = wait_for_response(events, 1, args.timeout)
    print(f"session/new response t+{session_response['elapsed']:.3f}s")

    session_id = session_response["message"].get("result", {}).get("sessionId")
    if session_id:
        print(f"sessionId={session_id}")

    try:
        proc.terminate()
        proc.wait(timeout=5)
    except Exception:
        proc.kill()

    return 0


def send_json(proc: subprocess.Popen, payload: dict) -> None:
    assert proc.stdin is not None
    proc.stdin.write(json.dumps(payload) + "\n")
    proc.stdin.flush()


def wait_for_response(events: queue.Queue, expected_id: int, timeout: float) -> dict:
    deadline = time.perf_counter() + timeout
    stderr_lines = []
    while time.perf_counter() < deadline:
        remaining = max(0.01, deadline - time.perf_counter())
        try:
            kind, seen_at, payload = events.get(timeout=remaining)
        except queue.Empty:
            continue

        if kind == "stderr":
            stderr_lines.append((seen_at, payload))
            continue

        if kind != "stdout":
            continue

        try:
            message = json.loads(payload)
        except json.JSONDecodeError:
            continue

        if message.get("id") == expected_id:
            return {
                "elapsed": seen_at,
                "message": message,
                "stderr": stderr_lines,
            }

    raise TimeoutError(f"timed out waiting for response id={expected_id}")


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except Exception as exc:
        print(f"probe failed: {exc}", file=sys.stderr)
        raise
