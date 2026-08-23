# IPC 协议草案（verba-protos）

> 状态：草案 v0.1，随实现演进 · 关联：[架构设计](architecture.md) §5。

## 1. 传输层

- **Windows**：Named Pipe `\\.\pipe\verba-<uid>`（uid 为每用户实例后缀，避免多用户冲突）。
- **macOS / Linux**：Unix Domain Socket `$XDG_RUNTIME_DIR/verba.sock`，回退 `/tmp/verba.sock`。
- **帧格式**：`u32 LE 长度前缀 + Protobuf message`。
- **连接模型**：全双工；客户端发起 `Request`，服务端回 `Response`；流式结果由服务端主动推 `StreamEvent`（带请求 id 关联）。
- **并发**：单连接多请求并发，靠 `id` 关联；`id` 由客户端自增分配。

## 2. 消息模型（Protobuf，见 `crates/verba-protos/proto/verba.proto`）

```proto
message Request {
  uint64 id = 1;
  oneof kind {
    Ping ping = 10;                // 健康检查
    SetMode set_mode = 11;         // normal / voice / ocr / ai
    GetConfig get_config = 12;
    SetConfig set_config = 13;     // map<string,string>
    LlmGenerate llm_generate = 20; // // AI 模式：prompt + 参数，流式
    LlmCancel llm_cancel = 21;     // 取消流式生成
    LlmCandidates llm_candidates = 22; // 候选融合：为拼音补充 LLM 候选
    RimeCandidates rime_candidates = 23; // 可选 Rime 引擎候选（config 引擎=rime）
  }
}

message LlmCandidates {
  string pinyin = 1;               // 当前拼音串（如 "nishishui"）
  repeated string dictionary = 2;  // 已有词库候选（供 LLM 参考 / 去重）
  int32 max_candidates = 3;        // 最多生成数
}

// Rime 引擎候选（daemon 内 librime，同步返回一个 Candidates 事件 done=true）。
message RimeCandidates {
  string input = 1;                // 拼音/五笔码串
  string schema = 2;               // 方案（luna_pinyin_simp / wubi86 …）
  int32 max_candidates = 3;
}

message Response {
  uint64 id = 1;
  oneof kind {
    Pong pong = 2;
    Ok ok = 3;
    Error error = 4;               // code + message
    Text text = 5;                 // 一次性结果（OCR / ASR 整段，预留）
    Config config = 6;
  }
}

message StreamEvent {
  uint64 id = 1;                   // 关联请求 id
  oneof kind {
    Chunk chunk = 2;               // LLM token 增量
    Final final = 3;               // LLM 流结束的完整结果
    Error error = 4;
    Candidates candidates = 5;     // 候选融合增量
  }
}

message Candidates {
  string pinyin = 1;               // 回显请求拼音，客户端按当前组合校验过期结果
  repeated string candidates = 2;  // 本次新增候选（追加语义）
  bool done = 3;                   // 本次生成结束（含取消，保证客户端退出阻塞读）
}
```

## 3. 关键语义

- **SetMode**：模式切换（Normal / Voice / Ocr / Ai）。AI 模式进入后，前端把按键收集为 prompt 文本，直到 Enter 提交 / Esc 退出。
- **OcrImage**：支持 `bytes`（剪贴板 / 截图）或 `file_ref`（临时文件路径，避免大包传输；临时文件由请求方负责清理）。
- **LlmGenerate**：字段含 `provider`（空 = 默认）、`prompt`、`system`、`temperature`、`max_tokens`、`stream`（默认 true）。
- **LlmCandidates（候选融合）**：拼音态输入停顿后由前端发起，daemon 按行解析 LLM 输出为候选，
  增量推 `Candidates` 事件（去重 / 去编号），结束（含取消）补发 `done=true`。
- **RimeCandidates**：`config 引擎=rime` 时前端把拼音/五笔码发到 daemon，daemon 内 librime
  查询候选并一次性回 `Candidates`（`done=true`）；`rime_schema` 配置方案。
- **取消**：任何流式请求可 `LlmCancel` 按全局请求 id 中止；daemon 应尽快释放资源并补发结束事件，保证客户端退出阻塞读。
- **断线重连**：客户端检测连接断开后按退避重连，并重新同步当前模式与配置。
- **背压**：大文件 / 高吞吐用分块 + 流控，避免管道阻塞（参照数据通道背压经验）。

## 4. 版本演进

- 消息加字段只增不删不改语义；破坏性变更提升协议主版本并保留兼容别名。
- 新增能力先更新本草案 + `verba-protos`，再实现 daemon 与前端，保持契约同步。