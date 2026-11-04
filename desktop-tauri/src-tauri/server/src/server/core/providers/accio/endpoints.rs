//! Accio 的**地区**与端点常量：两个地区共用一套网关，只有登录站点不同。
//!
//! ── 上游长什么样（逆向来源：Accio Work 桌面端安装包的 app.asar）──────
//! ```text
//!   业务网关（国际 / 国内同一个）  https://phoenix-gw.alibaba.com
//!     /api/oauth/token                 授权码 → accessToken / refreshToken
//!     /api/auth/refresh_token          续期（body 带 accessToken + refreshToken）
//!     /api/auth/userinfo               用户资料（GET，accessToken 走 query）
//!     /api/entitlement/quota           额度用量百分比 + 重置倒计时
//!     /api/entitlement/currentSubscription  订阅详情
//!     /api/llm/config                  模型目录（POST，body {token}）—— 本网关用它
//!     /api/llm/config/v2               模型目录的精简版（字段更少，见下）
//!   推理网关（ADK）                https://phoenix-gw.alibaba.com/api/adk/llm
//!     POST /generateContent?sg_k=<md5(requestId)>   SSE，Gemini 风格信封
//!   登录站点
//!     国际版  https://www.accio.com      （域名 accio.com → 区域 GLOBAL）
//!     国内版  https://www.accio-ai.com   （域名 accio-ai.com → 区域 CN）
//! ```
//!
//! 三个事实决定了本模块的形状（都是实测/读码得到的，不是推测）：
//!   1. **网关是同一个** —— 两个地区的账号打的是同一个 host，差别只在
//!      `x-package-region`（`GLOBAL` / `CN`）与登录站点。因此「这一家的域名」
//!      不能是模块级常量，必须由 `Region` 给。
//!   2. **`client_id` 两地逐字相同**（`accio-work`）：桌面端两个构建共用它。
//!   3. **token 不走 Authorization 头**：业务接口的 GET 把 `accessToken` 拼进
//!      query、POST 放进 JSON body；推理接口把 `token` 放进 body。这一点与
//!      项目里其它七家都不同，见 `credentials.rs` 与 `chat.rs`。
//!
//! ── 为什么地区是一等公民而不是「一家上的一个字段」────────────────
//! 与 `autoclaw::region` / Cline 两池同一条思路：地区做成账号上的字段，
//! 界面与选路就分不清「哪条账号走哪个站点」。两个地区 = 两家 provider
//! （`accio` / `accio-cn`），共用这一份实现。

use crate::server::core::providers::{kind_id, ProviderKind};
use crate::server::errors::GatewayError;

/// 业务网关基址（环境变量 `ACCIO_GATEWAY_BASE` 可覆盖，预发/自建代理用）
pub const DEFAULT_GATEWAY_BASE: &str = "https://phoenix-gw.alibaba.com";

/// 推理网关路径前缀（拼在网关基址之后）
pub const ADK_LLM_PATH: &str = "/api/adk/llm";

/// OAuth 授权码换令牌
pub const OAUTH_TOKEN_PATH: &str = "/api/oauth/token";
/// 刷新令牌（多账号场景桌面端用 `/api/auth/safe/refresh_token`，本网关用普通那条）
pub const REFRESH_TOKEN_PATH: &str = "/api/auth/refresh_token";
/// 用户资料
pub const USER_INFO_PATH: &str = "/api/auth/userinfo";
/// 额度用量
pub const QUOTA_PATH: &str = "/api/entitlement/quota";
/// 订阅详情
pub const SUBSCRIPTION_PATH: &str = "/api/entitlement/currentSubscription";
/// 我加入了哪些团队（POST，body `{utdid, version, accessToken}`）→ `data: [{teamId, teamName, role, status, …}]`。
///
/// `userinfo.activeTeam` 只在服务端会话已经切过空间时才有值（实测多数账号是
/// `null`），所以「这个账号能进哪些公司空间」只能问这一条。见 `auth::adopt_single_team`。
pub const TEAMS_PATH: &str = "/api/team/queryUserTeams";
/// 模型目录（完整版）。
///
/// ── 为什么是 v1 而不是 v2（2026-09 实测）───────────────────
/// 两条路径都返回同一个信封（`data` 是 provider 数组），差别在**清单本身**：
/// 用国际版账号实测，v1 给 9 家 provider / 69 个模型 / 41 个可见，
/// v2 只给 6 家 / 46 个 / 26 个 —— v2 **少了 deepseek、moonshot、zhipu 三家**
/// （DeepSeek / Kimi / GLM 全都看不到）。此外 v1 每条模型多出 `protocol`
/// 与 `group` 两个字段，前者正是思考档位落点要用的（见 `models.rs`）。
/// 因此目录刷新打 v1，`MODEL_CONFIG_PATH_V2` 只作为回落。
pub const MODEL_CONFIG_PATH: &str = "/api/llm/config";
/// 模型目录的精简版（旧路径）。只在 v1 意外失败时回落一次，见 `models::refresh`
pub const MODEL_CONFIG_PATH_V2: &str = "/api/llm/config/v2";

/// OAuth 客户端 id（桌面端常量；两地逐字相同）
pub const CLIENT_ID: &str = "accio-work";

/// ADK 推理请求的租户（`ACCIO_TENANT` 可覆盖；**缺省是空串**）。
///
/// ── 为什么缺省不是 `accio-agent`（2026-10 实测，会直接封掉部分模型）──
/// 上一版照抄了 accio-manager 的默认值，代价是**一部分模型整类不可用**：上游对
/// `tenant=accio-agent` 回 `IAI ERR: tenant=accio-agent no permission to access
/// model=gemini-4-argon`（HTTP 200 + 帧内 error_code 505，网关翻成 502）。
/// 桌面端发的是**空串**（`ADK_TENANT` 未设即 `""`，见 sdk.log 里 `llm.call` 的
/// `input.tenant: ""`），上游按 token 自己解析租户。同一账号、同一 token、只把
/// `tenant` 由 `accio-agent` 换成空串：Gemini 4 Argon 立刻正常出流，DeepSeek 等
/// 原本可用的模型不受影响。
pub fn tenant() -> String {
    env_text("ACCIO_TENANT").unwrap_or_default()
}
/// ADK 推理请求的来源标记（桌面端默认值 `ADK_IAI_TAG || "phoenix-desktop"`）
pub const DEFAULT_IAI_TAG: &str = "phoenix-desktop";
/// 推理请求头里的客户端版本（桌面端取 appVersion；缺省给一个近版号）
pub const DEFAULT_APP_VERSION: &str = "0.32.6";

/// 推理请求头 `appKey`（**必填**，见下）。
///
/// ── 为什么它必须有（2026-09 实测）────────────────────────────
/// 不带这个头打 `generateContent`，上游**不报错**，而是回一段普通文本
/// ：「Your app version is no longer supported. Please update…」——
/// HTTP 200、`data:` 帧，形态与正常回答完全一样，会被当成模型输出吐给下游。
/// 带上任意**非空**取值即恢复（实测 `35298846` / `12345678` / 任意字符串
/// 都通，空串等同于没带）—— 上游只校验「有没有」，不校验取值。
/// 取的是 accio-manager 的默认值，与桌面端同源。`ACCIO_APP_KEY` 可覆盖。
pub const DEFAULT_APP_KEY: &str = "35298846";

/// 业务请求的默认语言（桌面端按界面语言给 `zh` / `en`，网关侧固定 en ——
/// 额度与目录的文案不参与界面展示，语言只影响上游返回的消息文本）
pub const ACCEPT_LANGUAGE: &str = "en";

/// Accio 的两个地区。**两个地区 = 两个 provider**（见模块头）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Region {
    /// 国际版（`www.accio.com`，`x-package-region: GLOBAL`）
    Global,
    /// 国内版（`www.accio-ai.com`，`x-package-region: CN`）
    Cn,
}

impl Region {
    /// 全部地区（缓存数组、遍历用）
    pub const ALL: [Region; 2] = [Region::Global, Region::Cn];

    /// 本地区的 provider
    pub const fn kind(self) -> ProviderKind {
        match self {
            Self::Global => ProviderKind::Accio,
            Self::Cn => ProviderKind::AccioCn,
        }
    }

    /// 本地区的 provider id（`"accio"` / `"accio-cn"`）
    pub fn provider_id(self) -> &'static str {
        kind_id(self.kind())
    }

    /// provider id → 地区；不是 Accio 系时 None
    pub fn from_provider_id(provider_id: &str) -> Option<Self> {
        match provider_id {
            "accio" => Some(Self::Global),
            "accio-cn" => Some(Self::Cn),
            _ => None,
        }
    }

    /// provider kind → 地区；不是 Accio 系时 None
    pub fn from_kind(kind: ProviderKind) -> Option<Self> {
        match kind {
            ProviderKind::Accio => Some(Self::Global),
            ProviderKind::AccioCn => Some(Self::Cn),
            _ => None,
        }
    }

    /// 落进账号记录的 `edition` 值（与其它家同一套取值：`intl` / `cn`）
    pub const fn edition(self) -> &'static str {
        match self {
            Self::Global => "intl",
            Self::Cn => "cn",
        }
    }

    /// 界面与日志里的名字
    pub const fn label(self) -> &'static str {
        match self {
            Self::Global => "国际版",
            Self::Cn => "国内版",
        }
    }

    /// 解析前端传来的 `mode` / `edition` 取值。缺省 = 国际版
    /// （老客户端不带这个字段时行为必须稳定）。
    pub fn parse(value: &str) -> Result<Self, GatewayError> {
        match value.trim() {
            "" | "global" | "intl" => Ok(Self::Global),
            "cn" => Ok(Self::Cn),
            _ => Err(GatewayError::with_status(
                400,
                "Accio 地区必须为 global（国际版）或 cn（国内版）",
            )),
        }
    }

    /// 从账号记录 / 添加请求里取地区（兼容 `mode` 与 `edition` 两个键名）
    pub fn from_payload(payload: &serde_json::Value) -> Result<Self, GatewayError> {
        match payload.get("mode").or_else(|| payload.get("edition")) {
            None | Some(serde_json::Value::Null) => Ok(Self::Global),
            Some(serde_json::Value::String(value)) => Self::parse(value),
            _ => Err(GatewayError::with_status(400, "Accio 地区必须是字符串")),
        }
    }

    /// 登录站点（OAuth 授权页所在域；`ACCIO_LOGIN_BASE` 可覆盖，预发用）
    pub fn login_base(self) -> String {
        if let Some(value) = env_text("ACCIO_LOGIN_BASE") {
            return value.trim_end_matches('/').to_string();
        }
        match self {
            Self::Global => "https://www.accio.com".to_string(),
            Self::Cn => "https://www.accio-ai.com".to_string(),
        }
    }

    /// 模型目录与推理请求的 `x-package-region` 头。
    ///
    /// 取值来自桌面端：区域名单就是 `["CN", "GLOBAL"]` 两项
    /// （`domain/config` 的返回与 `packageRegion` 校验都用它）。
    pub const fn package_region(self) -> &'static str {
        match self {
            Self::Global => "GLOBAL",
            Self::Cn => "CN",
        }
    }
}

/// 业务网关基址（环境变量可覆盖；末尾不带斜杠）
pub fn gateway_base() -> String {
    env_text("ACCIO_GATEWAY_BASE")
        .unwrap_or_else(|| DEFAULT_GATEWAY_BASE.to_string())
        .trim_end_matches('/')
        .to_string()
}

/// 推理网关基址（`{gw}/api/adk/llm`）
pub fn llm_base() -> String {
    format!("{}{}", gateway_base(), ADK_LLM_PATH)
}

/// `appKey` 请求头的取值（`ACCIO_APP_KEY` 可覆盖）。**只用于推理接口**：
/// 业务接口（目录 / 额度 / 资料）实测带不带都一样，多带只是徒增风控面。
pub fn app_key() -> String {
    env_text("ACCIO_APP_KEY").unwrap_or_else(|| DEFAULT_APP_KEY.to_string())
}

/// 推理请求的 `User-Agent`（`ACCIO_USER_AGENT` 可覆盖）。
///
/// ── 为什么必须显式给，而且不能沿用全局默认（2026-10 实测，可复现）──
/// `egress` 的客户端默认 UA 是 `undici`（计费接口缺 UA 会 403，见那边模块头），
/// 可阿里侧对 ADK 推理端点有一道风控规则**恰好把它拉黑**：同一个 token、同一台
/// 机器、同一组头，只在这个头与别的取值之间来回换，结果就跟着翻转 ——
///
/// ```text
///   User-Agent: undici            → HTTP 200 + 约 1.2 KB 的 punish 拦截页（无 SSE 帧）
///   User-Agent: curl/7.88.1       → 正常出流
///   User-Agent: accio-work/0.32.6 → 正常出流
///   （完全不发这个头）             → 正常出流
/// ```
///
/// 桌面端这条链路**不发 UA**（ADK SDK 走 `undici.request` 而非 `fetch`，前者不注入
/// `user-agent`），所以从没踩到这条规则；网关的客户端级默认头又必须保留（计费链路
/// 靠它），于是只能在**推理请求上逐条覆盖**。取值与自家客户端同形
/// （`<client_id>/<版本号>`）：既不是浏览器 UA，也不在任何风控黑名单上。
pub fn user_agent() -> String {
    env_text("ACCIO_USER_AGENT")
        .unwrap_or_else(|| format!("{}/{}", CLIENT_ID, DEFAULT_APP_VERSION))
}

/// 读一个环境变量并去掉空白；空串按「没设」处理
fn env_text(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// 业务接口的通用请求头（桌面端在拦截器里统一加的那一组里我们认得全的部分）。
///
/// ── 为什么只带这几项 ────────────────────────────────────────
/// 桌面端还带 `x-source` / `x-deploy-target` / `x-cna` / `bx-ua` 等，
/// 其中 `bx-ua` 由它内置的风控 SDK 生成（本网关没有那个 SDK，值也不可复现），
/// `x-cna` 是设备指纹、`x-deploy-target` 是部署形态标记 —— 都不是鉴权项。
/// **刻意不带 `Authorization`**：桌面端一个请求都不带它，鉴权全靠 body/query
/// 里的 `accessToken`；多一个上游没见过的头只会徒增风控面。
/// 未验证项与实测口径见 `mod.rs` 模块头的「未验证项」一节。
pub fn api_headers(region: Region, team_id: Option<&str>) -> Vec<(String, String)> {
    let mut headers = vec![
        ("Content-Type".to_string(), "application/json".to_string()),
        ("Accept".to_string(), "application/json".to_string()),
        ("Accept-Language".to_string(), ACCEPT_LANGUAGE.to_string()),
        ("x-language".to_string(), ACCEPT_LANGUAGE.to_string()),
        ("x-platform".to_string(), "desktop".to_string()),
        ("x-os".to_string(), std::env::consts::OS.to_string()),
        (
            "x-app-version".to_string(),
            env_text("ACCIO_APP_VERSION").unwrap_or_else(|| DEFAULT_APP_VERSION.to_string()),
        ),
        ("x-package-region".to_string(), region.package_region().to_string()),
    ];
    push_team_header(&mut headers, team_id);
    headers
}

/// 空间（公司空间）标识头 `x-team-id`。
///
/// ── 为什么是「可选追加」而不是固定字段（2026-10 实测）──────────
/// 个人空间下桌面端**根本不会带这个头**（拦截器里 `t && !e.headers['x-team-id']
/// && delete e.headers['x-team-id']` —— 空值会被删掉），所以这里的口径必须与它
/// 一致：只有 teamId **非空**才追加，绝不落一个空值。带空值不是「等价于个人
/// 空间」，而是给上游一个它从没见过的形态，平白多一层风控面。
///
/// 取值是**纯团队号**（如 `37001`），不是本地目录名里的 `账号_团队` 复合串。
pub fn push_team_header(headers: &mut Vec<(String, String)>, team_id: Option<&str>) {
    if let Some(team) = team_id.map(str::trim).filter(|value| !value.is_empty()) {
        headers.push(("x-team-id".to_string(), team.to_string()));
    }
}
