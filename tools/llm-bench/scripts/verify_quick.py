"""Exercise the CLI with a local HTTP fixture, without external API calls.

Run: uv run python tools/llm-bench/scripts/verify_quick.py
Build first. --binary can select another build or a Windows .exe.
"""
import argparse
import json
from datetime import datetime, timedelta
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
import subprocess
import tempfile
import threading


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path)
    args = parser.parse_args()
    crate = Path(__file__).resolve().parents[1]
    binary = args.binary or crate / "target/debug/llm-bench"
    if not binary.exists() and binary.with_suffix(".exe").exists():
        binary = binary.with_suffix(".exe")
    binary = binary.resolve(strict=True)
    requests = []
    judge_mode = "pass"

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_POST(self):
            body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            requests.append((self.path, body))
            status = 200
            if self.path == "/judge/systemone":
                assert list(body["questions"]) == ["basic"]
                assert "provider" not in body["state"]
                assert "focus" in body["state"] and "correction_prompt" in body["state"]
                choice = "uncertain" if judge_mode == "uncertain" else "pass"
                response = {"answers": {"basic": {"choice": choice, "confidence": 0.4 if judge_mode == "low" else 0.9,
                    "probabilities": {"pass": 0.9 if choice == "pass" else 0.05,
                                      "needs_revision": 0.05, "uncertain": 0.9 if choice == "uncertain" else 0.05}}}}
                if judge_mode == "malformed":
                    response = {"answers": {}}
                elif judge_mode == "unavailable":
                    status, response = 503, {"error": "judge unavailable"}
            else:
                model = body["model"]
                text = body["messages"][-1]["content"]
                status = 503 if model == "failure" else 200
                content = " " if model == "empty" else text
                response = {"error": "fixture failure"} if status != 200 else {
                    "choices": [{"message": {"content": content}}], "usage": {"total_tokens": 12}}
            data = json.dumps(response).encode()
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    worker = threading.Thread(target=server.serve_forever, daemon=True)
    worker.start()
    try:
        with tempfile.TemporaryDirectory(prefix="voicex-quick-") as folder:
            root = Path(folder)
            port = server.server_port
            # An obsolete shortlist must not hide configured models; legacy rounds apply only to --full.
            config = 'quick_providers = ["good"]\nrounds = 7\nprompt = "整理 {{DICTIONARY}}"\ndictionary = "Abaqus"\n'
            for name in ["good", "twin", "failure", "empty"]:
                config += f'\n[[provider]]\nname = "{name}"\nmodel = "{name}"\nbase_url = "http://127.0.0.1:{port}/v1"\napi_key = "local-fixture"\n'
            config += f'\n[judge]\nname = "judge"\nmodel = "fixture"\ntype = "typesafe"\nbase_url = "http://127.0.0.1:{port}/judge"\napi_key = "local-fixture"\n'
            # Secondary reviewer is never used or even resolved.
            config += '\n[hallucination_reviewer]\nname = "reviewer"\nmodel = "reviewer"\nbase_url = "unused"\napi_key = "file:missing-reviewer-key"\n'
            (root / "config.toml").write_text(config, encoding="utf-8")
            cases = ''.join(f'[[case]]\nname = "case-{i}"\ninput = "嗯，保留 GPT 6.1，第 {i} 个。"\nexpected = "保留 GPT 6.1，第 {i} 个。"\n\n' for i in range(3))
            for name in ["quick_cases.toml", "test_cases.toml"]:
                (root / name).write_text(cases, encoding="utf-8")
            standings = root / "standings.json"
            standings.write_text('{"untouched": true}', encoding="utf-8")

            def run(*flags, ok=True):
                result = subprocess.run([str(binary), *flags], cwd=root, capture_output=True, text=True, encoding="utf-8", timeout=30)
                assert (result.returncode == 0) == ok, result.stdout + result.stderr
                return result.stdout + result.stderr

            assert "12 correction calls" in run("--dry-run")
            assert not requests and not (root / "runs").exists()
            for flags in [("--provider", "typo"), ("--rounds", "--full"),
                          ("--quick", "--replay", "old.json"), ("--full", "--speed-only")]:
                run(*flags, ok=False)
            assert not requests

            output = run()
            assert "排名（本次小样本）" in output and "中位(ms)" in output
            assert len(requests) == 15  # 4 models x 3 cases, then 3 deduplicated judge calls
            corrections = [b for p, b in requests if p == "/v1/chat/completions"]
            assert len(corrections) == 12 and {b["model"] for b in corrections} == {"good", "twin", "failure", "empty"}
            saved = next((root / "runs").glob("*/results.json"))
            data = json.loads(saved.read_text(encoding="utf-8"))
            assert data["run"]["rounds"] == 1 and len(data["ranking"]) == 4
            assert data["run"]["execution"]["judge_calls"] == 3 and data["run"]["execution"]["reviewer_calls"] == 0
            stats = {s["provider"]: s for s in data["summary"]}
            assert stats["good"]["passes"] == stats["twin"]["passes"] == 3
            for name in ["failure", "empty"]:
                assert stats[name]["failures"] == 3 and stats[name]["median_ms"] is None
            assert "local-fixture" not in saved.read_text(encoding="utf-8")

            quick_history = root / "quick-standings.json"
            assert quick_history.exists() and data["standings"]["counted_runs"] == 1
            assert "滚动排行榜" in output
            start = len(requests)
            run("--standings-results", str(saved), "--full", ok=False)
            output = run("--standings-results", str(saved))
            assert "1 轮" in output and len(requests) == start
            history = json.loads(quick_history.read_text(encoding="utf-8"))
            assert len(history["runs"]) == 1
            # Import a second fixture race; viewing it repeatedly must not award points twice.
            second = json.loads(saved.read_text(encoding="utf-8"))
            second["run"]["at"] = (datetime.fromisoformat(second["run"]["at"]) + timedelta(seconds=1)).isoformat()
            second_path = root / "runs/second/results.json"
            second_path.parent.mkdir()
            second_path.write_text(json.dumps(second), encoding="utf-8")
            output = run("--standings-results", str(second_path))
            assert "2 轮" in output and "滚动排行榜" in output and len(requests) == start
            run("--standings-results", str(second_path))
            assert len(json.loads(quick_history.read_text(encoding="utf-8"))["runs"]) == 2
            # The snapshot for the earlier source must not include the later race.
            assert "1 轮" in run("--standings-results", str(saved))
            history_before = quick_history.read_bytes()
            run("--provider", "good", "--case", "case-0", "--no-log", "--no-standings", "--output", "no-standing.json")
            assert quick_history.read_bytes() == history_before
            assert "standings" not in json.loads((root / "no-standing.json").read_text(encoding="utf-8"))

            start = len(requests)
            run("--provider", "good", "--case", "case-1", "--rounds", "2", "--no-log", "--output", "retry.json")
            assert len(requests) - start == 3  # two corrections, one shared verdict
            retried = json.loads((root / "retry.json").read_text(encoding="utf-8"))
            assert len(retried["cases"]) == 1 and len(retried["cases"][0]["rounds"]) == 2

            for mode in ["uncertain", "malformed", "unavailable"]:
                judge_mode = mode
                start = len(requests)
                output = run("--provider", "good", "--case", "case-0", "--no-log", "--output", "pending.json")
                assert len(requests) - start == 2  # never retries
                pending = json.loads((root / "pending.json").read_text(encoding="utf-8"))
                assert pending["ranking"][0]["place"] is None and pending["ranking"][0]["pending"] == 1
                assert "待定" in output

            judge_mode = "low"
            output = run("--provider", "good", "--case", "case-0", "--no-log", "--output", "low.json")
            low = json.loads((root / "low.json").read_text(encoding="utf-8"))
            assert low["ranking"][0]["place"] == 1 and low["ranking"][0]["passes"] == 1
            assert low["ranking"][0]["low_confidence"] == 1 and "低置信，建议核对" in output
            judge_mode = "pass"
            no_judge = config.split("\n[judge]")[0]
            (root / "no-judge.toml").write_text(no_judge, encoding="utf-8")
            start = len(requests)
            run("--config", "no-judge.toml", ok=False)
            assert len(requests) == start
            run("--config", "no-judge.toml", "--speed-only", "--provider", "good", "--case", "case-0", "--no-log", "--output", "speed.json")
            speed = json.loads((root / "speed.json").read_text(encoding="utf-8"))
            assert len(requests) == start + 1 and speed["run"]["execution"]["judge_calls"] == 0
            assert speed["run"]["quality_evaluation"] == "not_performed"
            assert "84 correction calls" in run("--full", "--dry-run")
            run("--full", "--config", "no-judge.toml", "--provider", "good", "--case", "case-0", "--rounds", "1", "--no-standings", "--no-log", "--output", "full.json")
            full = json.loads((root / "full.json").read_text(encoding="utf-8"))
            assert len(full["ranking"]) == 1 and full["run"]["scoring"] == 10
            assert not list(root.rglob("*.html"))
            assert standings.read_text(encoding="utf-8") == '{"untouched": true}'
            print("PASS: all models by default, CLI ranking, deduplicated basic judgments, pending/error visibility, no retry/reviewer/HTML, selection, rolling history/import/deduplication, speed-only and legacy full mode")
    finally:
        server.shutdown()
        server.server_close()
        worker.join(timeout=5)


if __name__ == "__main__":
    main()
