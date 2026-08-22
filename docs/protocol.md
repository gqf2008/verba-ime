# IPC 协议草案（verba-protos）

> 状态：草案 v0.1，随实现演进 · 关联：[架构设计](architecture.md) §5。

## 1. 传输层

- **Windows**：Named Pipe `\\.\pipe\verba-<uid>`（uid 为每用户实例后缀，避免多用户冲突）。
- **macOS / Linux**：Unix Domain Socket `$XDG_RUNTIME_DIR/verba.sock`，回退 `/tmp/verba.sock`。
- **帧格式**：`u32 LE 长度前缀 + Protobuf message`。
- **连接模型**：全双工；客户端发起 `Request`，服务端回 `Response`；流式结果由服务端主动推 `StreamEvent`（带请求 id 关联）。
- **并发**：单连接多请求并发，靠 `id` 关联；`id` 由客户端自增分配。

## 2. 消息模型（Protobuf 草案）

```proto
message Request {
  uint64 id = 1;
  oneof kind {
    CommitText commit_text = 10;   // 直接上屏（前端本地也可直接完成，daemon 用于记录/统计）
    SetMode set_mode = 11;         // normal / voice / ocr / ai
    OcrImage ocr_image = 20;       // 图片（bytes 或文件引用）
    AsrStart asr_start = 21;       // 开始语音输入
    AsrStop asr_stop = 22;         // 结束语音输入
    LlmGenerate llm_generate = 30; // prompt + 参数
    LlmCancel llm_cancel = 31;     // 取消流式生成
    TtsSpeak tts_speak = 40;
    TtsStop tts_stop = 41;
    GetConfig get_config = 50;
    SetConfig set_config = 51;
    Ping ping = 60;                // 健康检查
  }
}

message Response {
  uint64 id = 1;
  oneof kind {
    Ok ok = 2;
    Err err = 3;                   // code + message
    Text text = 4;                 // 一次性结果（OCR / ASR 整段）
    Config config = 5;
    Pong pong = 6;
  }
}

message StreamEvent {
  uint64 id = 1;                   // 关联请求 id
  oneof kind {
    Chunk chunk = 2;               // LLM token / ASR 增量文本
    Final final_text = 3;          // 流结束的完整结果
    Progress progress = 4;         // 进度（如模型下载 / OCR 阶段）
    Cancelled cancelled = 5;
  }
}
```

## 3. 关键语义

- **SetMode**：模式切换（Normal / Voice / Ocr / Ai）。AI 模式进入后，前端把按键收集为 prompt 文本，直到 Enter 提交 / Esc 退出。
- **OcrImage**：支持 `bytes`（剪贴板 / 截图）或 `file_ref`（临时文件路径，避免大包传输；临时文件由请求方负责清理）。
- **LlmGenerate**：字段含 `provider`（空 = 默认）、`prompt`、`system`、`temperature`、`max_tokens`、`stream`（默认 true）。
- **取消**：任何流式请求可 `LlmCancel` / `AsrStop` 中止；daemon 应尽快释放资源。
- **断线重连**：客户端检测连接断开后按退避重连，并重新同步当前模式与配置。
- **背压**：大文件 / 高吞吐用分块 + 流控，避免管道阻塞（参照数据通道背压经验）。

## 4. 版本演进

- 消息加字段只增不删不改语义；破坏性变更提升协议主版本并保留兼容别名。
- 新增能力先更新本草案 + `verba-protos`，再实现 daemon 与前端，保持契约同步。