# 3.4.0 + 自编译「提供商增强」插件（Accio 公司空间）

## 这套部署是什么

3.4.0 起所有提供商实现从本仓库搬到了**插件**（`agent2api-providers`）。官方插件不带
Accio 的这几处修复，所以这里的做法是：**用公开的插件源码编一份带补丁的插件**，装进 3.4.0 网关。

- 插件源码：`https://github.com/ProvidersPlugin/Agent2Providers`（tag `v3.4.0`，公开）
- 补丁本身：`patches/accio-teamspace.patch`（同一个仓库里 `feat/accio-teamspace` 分支的提交）
- 补丁内容：
  1. 公司空间：账号/凭证新增 `teamId` / `teamName`，推理请求带 `x-team-id`
  2. `tenant` 缺省留空（原写死 `accio-agent`，会让部分模型被判无权限）
  3. 工具调用 id：从 `raw_response_json...functionCall.id` 提取，缺失时生成轮内唯一 id
  4. 工具结果回填 `function_response.name`（按 `tool_call_id` 反查）
  5. 同一 `function_call` part 被上游重放时不再当成两次调用

## 目录与端口

| 用途 | 路径 / 值 |
|---|---|
| 网关源码（3.4.0） | `E:\Program\DOCKER\agent2api-340` |
| 插件源码 | `E:\Program\DOCKER\Agent2Providers`（分支 `feat/accio-teamspace`） |
| 数据目录 | `E:\Program\DOCKER\agent2api-340\data340` |
| 插件包 | `data340\plugins\agent2api-providers\<version>\` |
| 网关镜像 | `agent2api:3.4.0-teamspace` |
| 面板 / API | `http://localhost:3066/` ｜ `http://localhost:3066/v1` |
| 旧版本（已归档） | 容器 `agent2api`（停止，`--restart=no`），数据 `E:\Program\DOCKER\agent2api-3.0.8-archive`，镜像 `agent2api:3.0.8-teamspace-stable`，git tag `stable/3.0.8-teamspace` |

## 重建插件

```powershell
cd E:\Program\DOCKER\agent2api-340
.\scripts\rebuild-accio-plugin.ps1 -Version 3.4.1
```

脚本做四件事：在 `rust:1-bookworm` 容器里 `cargo build --no-default-features --features accio` →
拷进 `plugins/agent2api-providers/<version>/` → 写 `plugins.json` 登记项（`signed=false` + 真实 sha256）→ 重启网关。

## 三个坑

1. **必须带版本目录**：网关按 `plugins/<id>/<version>/pack.json` 找，平铺会报「清单读不出来」。
2. **必须登记 `plugins.json`**：没登记时网关会去官方索引自动下载官方包，**覆盖补丁**。
   这里靠「本地版本号（3.4.1）高于官方（3.4.0）」压住；官方若发 3.4.1+ 会被替换。
3. **Dockerfile 首行**：`# syntax=docker/dockerfile:1` 需要从 Docker Hub 拉 frontend 镜像，
   本机不通，已在本分支删掉该行（改注释），否则镜像编不出来。

## 与 3.0.8 的差异（升级影响）

- 数据卷 schema 15 → 23，**单向**；升级前请整体备份 `data`（含 `account.key`，两者要一起备）。
- 插件包未签名，网关会标黄；功能不受影响。
- 3.4.0 新增：插件架构、路由工作台、OpenAI Images、会话图片压缩、网关 Key 强制等。
