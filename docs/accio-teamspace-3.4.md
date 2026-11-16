# 3.4.0 + 自编译「提供商增强」插件（Accio 公司空间）

## 这套部署是什么

3.4.0 起所有提供商实现从本仓库搬到了**插件**（`agent2api-providers`）。官方插件不带
Accio 的这几处修复，所以这里的做法是：**用公开的插件源码编一份带补丁的插件**，装进 3.4.0 网关。

- 插件源码（上游）：`https://github.com/ProvidersPlugin/Agent2Providers`（tag `v3.4.0`，公开）
- **带补丁的分支**：`https://github.com/stafly/Agent2Providers/tree/feat/accio-teamspace`（提交 `4884368`）——要重建就 `git clone` 这个分支，别用上游 tag
- 补丁文件：`patches/accio-teamspace.patch`（同一份改动的 patch 形态，便于离线或对着上游 tag 打）
- 补丁内容：
  1. 公司空间：账号/凭证新增 `teamId` / `teamName`，推理请求带 `x-team-id`
  2. `tenant` 缺省留空（原写死 `accio-agent`，会让部分模型被判无权限）
  3. 工具调用 id：从 `raw_response_json...functionCall.id` 提取，缺失时生成轮内唯一 id
  4. 工具结果回填 `function_response.name`（按 `tool_call_id` 反查）
  5. 同一 `function_call` part 被上游重放时不再当成两次调用

## 目录与端口

| 用途 | 路径 / 值 |
|---|---|
| 网关源码（当前 3.4.2） | `E:\Program\DOCKER\agent2api-342` |
| 插件源码 | `E:\Program\DOCKER\Agent2Providers`（分支 `feat/accio-teamspace-342`） |
| 数据目录 | `E:\Program\DOCKER\agent2api-342\data342` |
| 插件包 | `data342\plugins\agent2api-providers\3.4.3\` |
| 网关镜像 | `agent2api:3.4.2-teamspace` |
| 容器 | `agent2api-342`（`--restart unless-stopped`），3066 → 3065 |
| 面板 / API | `http://localhost:3066/` ｜ `http://localhost:3066/v1` |
| 上一版 3.4.0（已归档） | 容器 `agent2api-340`（停止，`--restart=no`），数据 `agent2api-340\data340`，镜像 `agent2api:3.4.0-teamspace` |
| 初版 3.0.8（已归档） | 容器 `agent2api`（停止，`--restart=no`），数据 `E:\Program\DOCKER\agent2api-3.0.8-archive`，镜像 `agent2api:3.0.8-teamspace-stable`，git tag `stable/3.0.8-teamspace` |

## 重建插件

```powershell
cd E:\Program\DOCKER\agent2api-342
.\scripts\rebuild-accio-plugin.ps1 -Version 3.4.3 -PluginSrc E:\Program\DOCKER\Agent2Providers -DataDir E:\Program\DOCKER\agent2api-342\data342 -Container agent2api-342
```

脚本做四件事：在 `rust:1-bookworm` 容器里 `cargo build --no-default-features --features accio` →
拷进 `plugins/agent2api-providers/<version>/` → 写 `plugins.json` 登记项（`signed=false` + 真实 sha256）→ 重启网关。

## 三个坑

1. **必须带版本目录**：网关按 `plugins/<id>/<version>/pack.json` 找，平铺会报「清单读不出来」。
2. **必须登记 `plugins.json`**：没登记时网关会去官方索引自动下载官方包，**覆盖补丁**。
   这里靠「本地版本号（3.4.3）高于官方（3.4.2）」压住；官方发更高版本时会被替换，届时把版本号再加一位重打包。
3. **Dockerfile 首行**：`# syntax=docker/dockerfile:1` 需要从 Docker Hub 拉 frontend 镜像，
   本机不通，已在本分支删掉该行（改注释），否则镜像编不出来。

## 3.4.2 这次升了什么（2026-11-16）

上游 3.4.0 → 3.4.2 三个提交，504 个文件（+23897/−3204），主要是：

- **自定义路由组**：跨提供商 / 跨模型组合成一个对外模型 ID，带成员顺序与失败后备选
- **路由草稿预演**：不发请求、不耗额度，展示候选、跳过原因与执行路径
- **压缩路由**：ZCode 上下文压缩请求可单独指定模型
- **工具服务页**：网络搜索 + 视觉分析（Skill / MCP / API 三种接入）
- **客户端识别**、账号积分到期倒计时、Windows 服务 / systemd 一键更新、安装脚本改造
- 插件侧（v3.4.2）：Kiro / AStudio 接入、MonkeyCode 网页登录、WorkBuddy 搜索；**Accio 模块**顺便加了 `sgcookie` 与 `utdid` 兜底（风控加固），但**仍然没有**公司空间、`tenant` 仍写死 `accio-agent`、工具调用 id 仍未处理

把补丁打到 v3.4.2 的冲突只有 3 处（`chat.rs` / `credentials.rs` / `oauth.rs`，都是两边同时改同一段的「加字段 / 加头」），`protocol.rs` 与 `stream.rs` 干净合入。
对应分支：`feat/accio-teamspace-342`（提交 `38d203f`）。

## 与 3.0.8 的差异（升级影响）

- 数据卷 schema 15 → 23，**单向**；升级前请整体备份 `data`（含 `account.key`，两者要一起备）。
- 插件包未签名，网关会标黄；功能不受影响。
- 3.4.0 新增：插件架构、路由工作台、OpenAI Images、会话图片压缩、网关 Key 强制等。
