# Music Maker OmniBridge 模型策略

本文定义 Music Maker 的业务角色、Project Profile 和模型调用边界。它补充工作区统一规则，
不复制 OmniBridge 的 Provider、Deployment、密钥、Base URL、上游模型或候选顺序。

## 1. 业务边界

Music Maker 不是单一的“音乐生成页面”，而是一条 AI 音乐制作流程：

1. 歌曲策划：从用户想法形成风格、主题、情绪和结构。
2. 文案创作：生成或润色歌词、歌曲描述、标题和封面 brief。
3. 音乐生成：云端 MiniMax Music 任务，以及未来的 Compute Hub 本地任务。
4. 成果管理：保存任务、Receipt、音频 Artifact 和可恢复状态。
5. 后期制作：分轨、转录、歌词时间轴、封面和导出；这些能力各自独立迁移。

项目负责业务 Prompt、输入约束、输出 schema、产品 UI 和业务结果。OmniBridge 负责模型能力目录、
Route 解析、Provider/Deployment 选择、凭据、候选顺序、安全 fallback 和调用 Receipt。

## 2. Project Profile v2

权威项目声明位于 `.omnibridge/project-profile.json`：

| 业务角色 | 能力 | 选择器 | 模式 | 用途 |
|---|---|---|---|---|
| `song_concept_draft` | text | 继承 `route:text:fast` | stream | 创意扩写与策划草稿 |
| `lyrics_draft` | text | `route:text:quality` | stream | 歌词初稿 |
| `lyrics_refine` | text | `route:text:quality` | stream | 歌词润色与结构修订 |
| `music_prompt_structuring` | text | `route:text:quality` | stream | 生成 Music 3 结构化描述 |
| `title_cover_brief` | text | 继承 `route:text:fast` | stream | 标题与封面 brief |
| `music_generation_cloud` | music | 继承 `route:music:default` | durable | 云端 MiniMax Music 任务 |

文本默认要求 `chat_completions` 与 `streaming`，使用 `latest_published` 读取当前已发布 Route。
音乐生成是非幂等 durable 任务，使用 `pinned_for_run` 冻结一次运行的 Route revision。

角色覆盖只替换 selector；operation、required capabilities、mode 和 revision policy 继承该能力默认值。
Profile 不包含 UI 文案、Prompt、Provider、密钥、Base URL、模型 ID 或候选顺序。

## 3. 自动、云端 API 与本地模型

三档是产品执行偏好，不是 Provider 选择器：

### 自动（推荐）

项目提交业务角色，由 Profile 解析通用 Route。OmniBridge 决定 Route 内的 Deployment 和安全
fallback。项目不能读取候选清单后自行重排，也不能把某个 Provider 写死为“自动”。

自动模式允许能力独立组合，例如：

- 云端文本模型完成歌曲策划与歌词；
- Compute Hub 本地 Music Worker 完成音乐生成；
- 某项本地能力不可用时，只对该能力执行已声明的策略，不影响另一能力的健康状态。

### 云端 API

文本角色使用 `route:text:fast` 或 `route:text:quality`；云端音乐使用
`music_generation_cloud -> route:music:default`。浏览器只调用 Music Maker 后端，Gateway key
和 durable task token 始终保留在受管服务端配置与私密任务存储中。

### 本地模型

本地音乐选项只有在本地能力已安装且 Worker task-ready 时才可选择。当前 Profile 只声明已发布的
云端音乐角色。Compute Hub Music Worker 接通后，应由 OmniBridge 发布领域中立的本地 Music
Route（例如 `route:music:local`；这里只是目标命名示例，不代表当前已经发布），再新增
`music_generation_local` 角色或执行策略绑定。

Music Maker 不保存 Worker ID、GPU 信息、workflow 路径或 Compute Hub 凭据，也不直连 Worker。
Compute Hub 是本地 durable 任务的唯一队列 owner；OmniBridge 提供统一父任务、Route 和 Receipt。

## 4. 文本与音乐状态独立

前端和后端必须分别表达：

- 文本策略是否可解析，流式能力是否可用；
- 云端 Music Route 是否可解析并具备 task-ready Deployment；
- 本地 Music Worker 是否已安装、在线并 task-ready；
- legacy cover、ASR 或本地兼容路径是否仍处于迁移状态。

因此，“本地音乐模型未安装”只能禁用本地音乐生成，不能禁用云端写作助手；文本 Route 故障也
不能被展示成 Music Route 故障。HTTP 200、Profile resolve 或任务 queued 都不能单独证明生成成功。

## 5. Receipt 与前端安全字段

项目可以保存并向授权 UI 展示以下脱敏信息：

- `request_id`
- 业务 `role_id`
- `selector_type`
- `route_id` 与 `route_revision`
- `resolved_provider`
- `resolved_deployment`
- `resolved_model`
- `provider_adapter`
- `attempt`
- 终态 outcome
- ArtifactRef、MIME、字节长度和 digest

不得进入浏览器、普通日志或公开结果 JSON：

- Gateway/Provider Authorization 或 API key
- durable task token
- Provider child task ID
- 私有或签名媒体 URL
- Base URL、账号信息或完整上游错误
- 未经授权的原始 Prompt、歌词或用户素材

Profile 解析预览与真实调用 Receipt 必须分开显示。解析预览没有调用模型，也不产生模型成功证据。

## 6. 失败、fallback 与恢复

同步或流式文本只有在 OmniBridge 能证明请求尚未送达、尚未开始输出且 Route 允许时，才可在冻结的
Route revision 内安全切换候选。4xx、鉴权错误、内容拒绝或 SSE 已开始后不得切换。

音乐 durable 提交流程必须保持：

```text
持久化 intent 和 payload digest
  -> POST 一次
  -> 得到 handle 后先私密持久化
  -> 只用 GET 查询或恢复
  -> 终态 Artifact / Receipt
```

超时、断连、5xx、`accepted` 或 `submission_unknown` 均不能自动重放 POST。没有 handle 时进入人工
核对；有 handle 时只查询原任务。项目不得自行切换 Provider 后重新生成。

## 7. 迁移边界

当前迁移分阶段完成：

1. 使用 Profile v2 固化文本和云端音乐业务角色。
2. 写作助手经项目 `model_port` 迁移到 OmniBridge Text Route，同时保留现有领域 Prompt/schema。
3. 云端音乐客户端改为通过 Profile 解析角色，同时保留 intent-before-submit 和 GET-only 恢复。
4. Compute Hub Music Worker task-ready 后，经 OmniBridge 增加本地 Music Deployment/Route。
5. cover、ASR 等能力分别登记 capability gap，再经对应 Route 迁移。

现有 OpenRouter、自托管服务器和本地兼容调用只能作为明确标记的 legacy 迁移边界。不得新增新的
直连，也不得在迁移完成前静默删除现有能力。任何真实付费生成都需要 owner 单独授权。

## 8. 无副作用验收

保存或发布前应先完成：

1. 使用当前 OmniBridge Project Profile v2 parser 做离线格式校验。
2. 调用 `/v1/project-profiles/validate` 和 `/v1/project-profiles/resolve` 做只读解析；两者不得发模型请求。
3. 确认每个角色的有效 selector、Route revision 和所需 capability。
4. 用 mock 验证 SSE Receipt、durable submit-once、unknown 零重放和 GET-only 恢复。
5. 只有 owner 明确授权后，才分别执行一次必要的真实文本或音乐调用。
