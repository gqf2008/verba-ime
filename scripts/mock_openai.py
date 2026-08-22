#!/usr/bin/env python3
"""Mock OpenAI 兼容 SSE 服务，用于本地开发/冒烟测试。

用法: python mock_openai.py [port]   （默认 8765）
POST /v1/chat/completions 返回流式 SSE（带简单 echo 逻辑）。
"""
import json
import sys
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


class Handler(BaseHTTPRequestHandler):
    def do_POST(self):
        length = int(self.headers.get("Content-Length", 0))
        self.rfile.read(length)
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-cache")
        self.end_headers()

        reply = "你好，我是本地 Mock LLM。Verba 输入法链路已打通 ✅"
        for i in range(0, len(reply), 2):
            chunk = reply[i : i + 2]
            payload = json.dumps(
                {"choices": [{"delta": {"content": chunk}}]}, ensure_ascii=False
            )
            self.wfile.write(f"data: {payload}\n\n".encode("utf-8"))
            self.wfile.flush()
            time.sleep(0.02)
        self.wfile.write(b"data: [DONE]\n\n")
        self.wfile.flush()

    def log_message(self, *args):
        pass


def main():
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 8765
    print(f"Mock OpenAI SSE server on http://127.0.0.1:{port}/v1", flush=True)
    ThreadingHTTPServer(("127.0.0.1", port), Handler).serve_forever()


if __name__ == "__main__":
    main()