#!/usr/bin/env python3
"""codegloss-lsp の textDocument/codeLens 応答時間を実プロセスで測る。

`docs/zed-display-notes.md` に載せている実測値はこのスクリプトの出力である。
Zed は使わない。リリースビルドしたサーバを子プロセスとして起動し、stdio 越しに
LSP を話して往復時間を測るだけなので、エディタが無い環境でも再現できる。

    cargo build --release -p codegloss-lsp
    python3 scripts/measure-code-lens.py

測るもの:

- cold  : didOpen 直後の codeLens。訳は 1 件も無く、全レンズがプレースホルダ。
- warm  : 訳が出そろったあとの codeLens。全レンズがキャッシュヒット。
- change: didChange の直後に codeLens を投げ、応答までを測る。コメントの
          再抽出（Tree-sitter の再パース）とレンズ生成の両方を含む。

`--lines` / `--rounds` で行数と繰り返し回数を変えられる。
"""

import argparse
import json
import statistics
import subprocess
import sys
import threading
import time

DEFAULT_BINARY = "target/release/codegloss-lsp"
URI = "file:///tmp/codegloss-bench/main.rs"

# didChange のあと、訳ができて refresh が飛ぶまでの待ち時間の上限。
REFRESH_TIMEOUT = 60.0


class Client:
    """LSP を stdio で話す最小のクライアント。"""

    def __init__(self, binary):
        self.process = subprocess.Popen(
            [binary],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
        )
        self.next_id = 1
        self.responses = {}
        self.server_requests = []
        self.event = threading.Condition()
        threading.Thread(target=self._read_loop, daemon=True).start()

    def _read_loop(self):
        stream = self.process.stdout
        while True:
            length = None
            while True:
                line = stream.readline()
                if not line:
                    return
                line = line.strip()
                if not line:
                    break
                name, _, value = line.decode().partition(":")
                if name.lower() == "content-length":
                    length = int(value.strip())
            if length is None:
                return
            message = json.loads(stream.read(length))
            with self.event:
                if "id" in message and "method" in message:
                    # サーバ発のリクエスト（workspace/*/refresh）。翻訳ワーカは
                    # 応答を待つので、その場で返さないと後続が止まる。
                    self.server_requests.append(message["method"])
                    self._write({"jsonrpc": "2.0", "id": message["id"], "result": None})
                elif "method" in message:
                    self.server_requests.append(message["method"])
                else:
                    self.responses[message["id"]] = message
                self.event.notify_all()

    def _write(self, message):
        body = json.dumps(message).encode()
        self.process.stdin.write(b"Content-Length: %d\r\n\r\n" % len(body) + body)
        self.process.stdin.flush()

    def notify(self, method, params):
        with self.event:
            self._write({"jsonrpc": "2.0", "method": method, "params": params})

    def request(self, method, params, timeout=60.0, started=None):
        """リクエストを送り (レスポンス, 秒) を返す。

        started を渡すと、そこからの経過時間を測る（didChange を送った時刻を
        起点にしたいときに使う）。
        """
        with self.event:
            request_id = self.next_id
            self.next_id += 1
            started = started or time.perf_counter()
            self._write(
                {"jsonrpc": "2.0", "id": request_id, "method": method, "params": params}
            )
            deadline = time.perf_counter() + timeout
            while request_id not in self.responses:
                remaining = deadline - time.perf_counter()
                if remaining <= 0:
                    raise TimeoutError(f"{method} の応答が来ない")
                self.event.wait(remaining)
            return self.responses.pop(request_id), time.perf_counter() - started

    def wait_for_refresh(self, timeout=REFRESH_TIMEOUT):
        """workspace/codeLens/refresh が来るまで待ち、待った秒数を返す。"""
        with self.event:
            started = time.perf_counter()
            deadline = started + timeout
            seen = 0
            while True:
                if "workspace/codeLens/refresh" in self.server_requests[seen:]:
                    return time.perf_counter() - started
                seen = len(self.server_requests)
                remaining = deadline - time.perf_counter()
                if remaining <= 0:
                    raise TimeoutError("codeLens/refresh が来ない")
                self.event.wait(remaining)

    def forget_refreshes(self):
        with self.event:
            self.server_requests.clear()

    def code_lens(self, started=None):
        return self.request(
            "textDocument/codeLens", {"textDocument": {"uri": URI}}, started=started
        )

    def close(self):
        try:
            self.request("shutdown", None, timeout=5)
            self.notify("exit", None)
            self.process.wait(timeout=5)
        except Exception:
            self.process.kill()


def source(lines, revision=0):
    """コメントの多い Rust ファイルを作る。

    1 単位 6 行で、ドキュメントコメント 2 行のブロックと行末コメント 1 つを
    含む。コメント本文はすべて異なるので、キャッシュも翻訳も 1 件ずつ効く。
    revision を変えると全コメントの本文が変わり、キャッシュが総入れ替えになる。
    """
    parts = ["//! Generated by scripts/measure-code-lens.py.\n\n"]
    index = 0
    written = 2
    while written + 6 <= lines:
        parts.append(
            f"/// Returns the cached value for item {index}, revision {revision}.\n"
            f"/// Falls back to the slow path when the cache is cold.\n"
            f"pub fn item_{index}(value: u32) -> u32 {{\n"
            f"    value + {index} // Adds the offset of item {index}, revision {revision}.\n"
            f"}}\n"
            f"\n"
        )
        index += 1
        written += 6
    # 先頭の `//!` も 1 ブロックとして数える。
    return "".join(parts), index * 2 + 1


def edited(text, round_number):
    """先頭のコメント 1 行だけを書き換えた版を返す（1 件だけキャッシュミス）。"""
    return text.replace(
        "//! Generated by scripts/measure-code-lens.py.",
        f"//! Generated by scripts/measure-code-lens.py, edit {round_number}.",
        1,
    )


def summarize(label, samples):
    ms = sorted(value * 1000 for value in samples)
    print(
        f"{label:<34} n={len(ms):<3} "
        f"min {ms[0]:7.2f} ms  median {statistics.median(ms):7.2f} ms  "
        f"max {ms[-1]:7.2f} ms"
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", default=DEFAULT_BINARY)
    parser.add_argument("--lines", type=int, default=1000)
    parser.add_argument("--rounds", type=int, default=20)
    arguments = parser.parse_args()

    text, blocks = source(arguments.lines)
    line_count = text.count("\n")
    print(f"file: {line_count} 行 / コメントブロック {blocks} 件")
    print(f"binary: {arguments.binary}")
    print()

    client = Client(arguments.binary)
    try:
        client.request(
            "initialize",
            {
                "processId": None,
                "rootUri": None,
                "capabilities": {
                    "workspace": {"codeLens": {"refreshSupport": True}},
                },
            },
        )
        client.notify("initialized", {})

        # cold: 訳が 1 件も無い状態。全レンズがプレースホルダになる。
        opened_at = time.perf_counter()
        client.notify(
            "textDocument/didOpen",
            {
                "textDocument": {
                    "uri": URI,
                    "languageId": "rust",
                    "version": 1,
                    "text": text,
                }
            },
        )
        cold_response, cold = client.code_lens()
        cold_lenses = cold_response["result"]
        placeholders = sum(
            1 for lens in cold_lenses if lens["command"]["title"] == "⟳ 翻訳中…"
        )
        print(f"cold codeLens: {cold * 1000:.2f} ms / レンズ {len(cold_lenses)} 件 "
              f"(プレースホルダ {placeholders} 件)")

        # 訳が出そろうまで待つ。debounce 150 ms + エンジン + refresh の往復。
        client.wait_for_refresh()
        settled = time.perf_counter() - opened_at
        print(f"didOpen から codeLens/refresh まで: {settled * 1000:.0f} ms "
              f"(うち 150 ms は debounce)")
        print()

        warm_samples = []
        for _ in range(arguments.rounds):
            response, elapsed = client.code_lens()
            warm_samples.append(elapsed)
            titles = [lens["command"]["title"] for lens in response["result"]]
            assert "⟳ 翻訳中…" not in titles, "warm のはずが未訳のレンズが残っている"

        change_samples = []
        refresh_samples = []
        for round_number in range(1, arguments.rounds + 1):
            client.forget_refreshes()
            changed = edited(text, round_number)
            started = time.perf_counter()
            client.notify(
                "textDocument/didChange",
                {
                    "textDocument": {"uri": URI, "version": round_number + 1},
                    "contentChanges": [{"text": changed}],
                },
            )
            response, elapsed = client.code_lens(started=started)
            change_samples.append(elapsed)

            titles = [lens["command"]["title"] for lens in response["result"]]
            # 書き換えた 1 件だけがキャッシュミスになる。これが出ているという
            # ことは、codeLens が didChange の後に処理された証拠でもある。
            assert titles.count("⟳ 翻訳中…") == 1, (
                "didChange が codeLens より先に処理されていない: "
                f"{titles.count('⟳ 翻訳中…')} 件が未訳"
            )
            refresh_samples.append(client.wait_for_refresh())

        summarize("warm codeLens (全件キャッシュヒット)", warm_samples)
        summarize("didChange -> codeLens 応答", change_samples)
        summarize("didChange -> codeLens/refresh", refresh_samples)
    finally:
        client.close()


if __name__ == "__main__":
    sys.exit(main())
