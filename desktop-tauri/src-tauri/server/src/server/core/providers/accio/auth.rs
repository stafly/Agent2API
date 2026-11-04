//! Accio 的请求封装与用户资料查询。
//!
//! ── 与另外几家的差别：token 不进请求头 ──────────────────────
//! 桌面端的业务接口**不走 Authorization**：GET 把 `accessToken` 拼进 query
//! （`GET /api/entitlement/quota?accessToken=…`），POST 放进 JSON body
//! （`POST /api/auth/refresh_token {accessToken, refreshToken}`）。本模块把
//! 这个约定收在两处构造里（[`get_with_token`] / [`post_json`]），别处不再各写。

use serde_json::{json, Value};

use crate::server::core::auth_http::{send_raw, ApiResponse};
use crate::server::core::proxies::{resolve_account_proxy, ProxyResolution, ResolvedProxy};
use crate::server::errors::GatewayError;
use crate::server::logging;

use super::credentials::{self, Credentials};
use super::endpoints::{self, Region};

/// 业务接口(非转发)的请求超时：与另外几家的 15 秒同量级
pub const REQUEST_TIMEOUT_MS: u64 = 15_000;

/// 一次出网请求（错误文案带地区名，便于排障时区分两站）
pub async fn request(
    method: &str,
    url: &str,
    body: Option<&Value>,
    headers: &[(String, String)],
    proxy: Option<&ResolvedProxy>,
) -> Result<ApiResponse, GatewayError> {
    send_raw(method, url, body, headers, proxy, Some(REQUEST_TIMEOUT_MS))
        .await
        .map_err(|error| {
            if error.is_timeout() {
                GatewayError::with_status(504, "Accio 请求超时，请检查网络后重试")
                    .with_code("accio_transport")
            } else {
                GatewayError::with_status(502, "无法连接 Accio，请检查网络或账号代理设置")
                    .with_code("accio_transport")
            }
        })
}

/// GET + token 进 query（业务接口的约定之一）。
///
/// `team_id` 是**公司空间**标识（个人空间给 `None`）：非空时追加 `x-team-id`，
/// 口径见 [`endpoints::push_team_header`]。
pub async fn get_with_token(
    region: Region,
    path: &str,
    token: &str,
    query: &[(&str, &str)],
    team_id: Option<&str>,
    proxy: Option<&ResolvedProxy>,
) -> Result<ApiResponse, GatewayError> {
    let mut url = url::Url::parse(&format!("{}{}", endpoints::gateway_base(), path))
        .map_err(|_| GatewayError::with_status(500, "Accio 请求地址无效"))?;
    {
        let mut pairs = url.query_pairs_mut();
        for (key, value) in query {
            pairs.append_pair(key, value);
        }
        if !token.is_empty() {
            pairs.append_pair("accessToken", token);
        }
    }
    let headers = endpoints::api_headers(region, team_id);
    request("GET", url.as_str(), None, &headers, proxy).await
}

/// POST + JSON body（业务接口的约定之二）。`team_id` 语义同 [`get_with_token`]。
pub async fn post_json(
    region: Region,
    path: &str,
    body: &Value,
    team_id: Option<&str>,
    proxy: Option<&ResolvedProxy>,
) -> Result<ApiResponse, GatewayError> {
    let url = format!("{}{}", endpoints::gateway_base(), path);
    let headers = endpoints::api_headers(region, team_id);
    request("POST", &url, Some(body), &headers, proxy).await
}

/// 把响应翻成「有效 JSON 对象」，失败时给出人话。
pub fn payload(response: ApiResponse, action: &str) -> Result<Value, GatewayError> {
    if !response.ok {
        let hint = match response.status {
            401 | 403 => "，请确认凭证有效或重新登录",
            429 => "，请稍后重试",
            _ => "",
        };
        return Err(GatewayError::with_status(
            i32::from(response.status),
            format!("Accio {action}失败（HTTP {}）{hint}", response.status),
        ));
    }
    response
        .payload
        .filter(Value::is_object)
        .ok_or_else(|| GatewayError::with_status(502, format!("Accio {action}未返回有效 JSON 对象")))
}

/// 账号级出口代理（解析失败按 400 报，不静默直连）
pub fn account_proxy(record: &Value) -> Result<Option<ResolvedProxy>, GatewayError> {
    match resolve_account_proxy(record.get("proxy")) {
        Some(ProxyResolution::Resolved(proxy)) => Ok(Some(proxy)),
        Some(ProxyResolution::Failed(reason)) => Err(GatewayError::with_status(400, reason)),
        None => Ok(None),
    }
}

/// 查用户资料（`POST /api/auth/userinfo`）。
///
/// ── 为什么是 POST（2026-10 实测修正）────────────────────────
/// 这个端点**只接受 POST**：`GET /api/auth/userinfo` 上游恒回
/// `405 Method Not Allowed`（与 token 无关，是方法级限制）。改造前这里走的是
/// `get_with_token`，于是「粘贴凭证」添加的账号**永远拿不到** userId / 昵称 /
/// 邮箱 —— 失败被 `prepare_account` 当「不影响添加」吞掉，只能退回凭证指纹
/// 兜底 id，界面上一律显示不出这是谁。桌面端与参考实现（accio-manager）都是
/// POST + body `{utdid, version, accessToken}`，本实现照此对齐。
///
/// 上游把资料放在 `data` 里还是顶层两种形态都见过（桌面端读的是
/// `data.user` / 顶层混合），这里都接受。
pub async fn fetch_profile(
    credentials: &Credentials,
    proxy: Option<&ResolvedProxy>,
) -> Result<Value, GatewayError> {
    let body = json!({
        "utdid": credentials.device_id,
        "version": endpoints::DEFAULT_APP_VERSION,
        "accessToken": credentials.access_token,
    });
    let response = post_json(
        credentials.region,
        endpoints::USER_INFO_PATH,
        &body,
        if credentials.is_team() { Some(credentials.team_id.as_str()) } else { None },
        proxy,
    )
    .await?;
    let payload = payload(response, "用户信息查询")?;
    if let Some((status, message)) = business_error(&payload) {
        return Err(GatewayError::with_status(
            status,
            format!("Accio 用户信息查询失败：{message}"),
        ));
    }
    Ok(unwrap_data(payload))
}

/// 业务层失败判定：Accio 的信封会在 **HTTP 200** 里表达失败。
///
/// ── 为什么必须单独判（2026-10 实测）─────────────────────────
/// 失效 token 打 `/api/entitlement/quota` 返回的是
/// `HTTP 200 + {"success":false,"code":"401","message":"NOT_LOGIN"}` ——
/// 只看 HTTP 状态会把它当成功，随后 `usagePercent` 取不到、默认成 0，
/// 界面上就变成「额度余量 100%」（改造前正是这个行为）。这一条把业务码翻成
/// 与 HTTP 语义对应的状态，让调用方原有的「401 → 刷新重试」链路能生效。
///
/// `success` 字段缺失时一律当成功（不是所有端点都带这个字段，不能凭缺失报错）。
pub fn business_error(payload: &Value) -> Option<(i32, String)> {
    if payload.get("success").and_then(Value::as_bool) != Some(false) {
        return None;
    }
    let code = match payload.get("code") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Number(number)) => number.to_string(),
        _ => String::new(),
    };
    let message = payload
        .get("message")
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
        .unwrap_or("上游返回业务错误")
        .to_string();
    let status = match code.as_str() {
        // 凭证问题：401 让「刷新后重试一次」的既有处置生效
        "401" | "403" | "NOT_LOGIN" => 401,
        // 限额：交给转发层的 QuotaLimited 档
        "429" => 429,
        // 其余业务失败：上游确实回了内容，但不是可用数据
        _ => 502,
    };
    Some((status, message))
}

/// 上游响应常见两层信封（`{success, data:{…}}`）；取内层
pub fn unwrap_data(payload: Value) -> Value {
    match payload.get("data") {
        Some(Value::Object(_)) => payload.get("data").cloned().unwrap_or(payload),
        _ => payload,
    }
}

/// 把资料里的身份/名字搬进凭证。
pub fn apply_profile(credentials: &mut Credentials, profile: &Value) {
    let user_id = credentials::text(profile, &["id", "userId", "user_id", "uid"]);
    if !user_id.is_empty() {
        credentials.user_id = user_id;
    }
    let name = credentials::text(profile, &["nickname", "name", "displayName", "userName"]);
    if !name.is_empty() {
        credentials.name = name.chars().take(100).collect();
    }
    let email = credentials::text(profile, &["email", "emailAddress"]);
    if !email.is_empty() {
        credentials.email = email.chars().take(320).collect();
    }
    let device_id = credentials::text(profile, &["deviceId", "device_id"]);
    if !device_id.is_empty() && credentials.device_id.is_empty() {
        credentials.device_id = device_id.chars().take(64).collect();
    }
    // 空间（公司空间）：上游若明确回报了当前空间是某个团队，就记下来。
    //
    // ── 为什么要顺手取（而不是只让用户手填）─────────────────────
    // 「当前空间」是**服务端会话**的属性，桌面端把它存进
    // `~/.accio/state/current-space.json`（`activeTeam: {teamId, teamName, role}`），
    // 网关这边没有那个文件，userinfo 是唯一能问到它的地方。只在「上游明确给了
    // 非空 teamId」且「本地还没有」时才采纳 —— 个人空间（activeTeam 为 null）
    // 不会被误写成团队，用户手填的值也不会被覆盖。
    if credentials.team_id.is_empty() {
        let team = profile
            .get("activeTeam")
            .or_else(|| profile.get("user").and_then(|user| user.get("activeTeam")))
            .filter(|value| value.is_object());
        if let Some(team) = team {
            let team_id = credentials::text(team, &["teamId", "team_id"]);
            if !team_id.is_empty() {
                credentials.team_id = team_id.chars().take(64).collect();
                let team_name = credentials::text(team, &["teamName", "team_name"]);
                if !team_name.is_empty() {
                    credentials.team_name = team_name.chars().take(100).collect();
                }
            }
        }
    }
}

/// 上游的「我加入了哪些团队」（`POST /api/team/queryUserTeams`）。
///
/// ── 为什么要问这一条 ────────────────────────────────────────
/// `userinfo.activeTeam` 只在**服务端会话已经切到某个空间**时才有值，实测多数
/// 账号给的是 `null`（桌面端把当前空间记在本地 `current-space.json`，不回落给
/// 服务端）。想知道「这个账号到底能进哪些公司空间」，可靠来源是这张团队列表：
/// 每条给 `{teamId, teamName, role, status, planId, …}`（实测 teamId 形如 `37001`）。
pub async fn fetch_teams(
    credentials: &Credentials,
    proxy: Option<&ResolvedProxy>,
) -> Result<Vec<Value>, GatewayError> {
    let body = json!({
        "utdid": credentials.device_id,
        "version": endpoints::DEFAULT_APP_VERSION,
        "accessToken": credentials.access_token,
    });
    let team = credentials.is_team().then_some(credentials.team_id.as_str());
    let response = post_json(credentials.region, endpoints::TEAMS_PATH, &body, team, proxy).await?;
    let payload = payload(response, "团队列表查询")?;
    if let Some((status, message)) = business_error(&payload) {
        return Err(GatewayError::with_status(
            status,
            format!("Accio 团队列表查询失败：{message}"),
        ));
    }
    // ── 信封形状：`data` 本身就是数组（实测）──────────────────────
    // `{"success":true,"code":"200","data":[{teamId,teamName,role,status,…}]}`。
    // 注意**不能**先走 `unwrap_data`：它只解「`data` 是对象」那一层，数组会被
    // 原样留在信封里，随后 `.as_array()` 恒为 None —— 于是「属于 0 个团队」这种
    // 假阴性会让自动绑定静默不生效（改造中真踩过一次）。
    let items = match payload.get("data").and_then(Value::as_array) {
        Some(items) => items.clone(),
        // 兼容其它两种信封：`data:{list:[…]}` / `data:{teams:[…]}`
        None => {
            let data = unwrap_data(payload);
            data.get("list")
                .and_then(Value::as_array)
                .cloned()
                .or_else(|| data.get("teams").and_then(Value::as_array).cloned())
                .unwrap_or_default()
        }
    };
    Ok(items)
}

/// 账号还没有空间时，替它认一次公司空间。
///
/// ── 规则：只有「恰好一个 ACTIVE 团队」才自动采纳 ─────────────
/// 这是**默认值**不是**强制值**：把用户塞进一个他自己没选的空间，等于让额度从
/// 另一个池子里扣。所以只在「他确实只属于一个团队、没有别的选择」时自动绑定；
/// 属于多个团队时留空（= 个人空间），由用户在面板 / API 上显式指定
/// （`PATCH /api/accounts/<id>` + `{"teamId":"…"}`）；想回到个人空间同样是一条
/// PATCH（`teamId: null`）。用户已经在 payload 里写了 teamId 的，这里不覆盖。
pub async fn adopt_single_team(credentials: &mut Credentials, proxy: Option<&ResolvedProxy>) {
    if credentials.is_team() {
        return;
    }
    let teams = match fetch_teams(credentials, proxy).await {
        Ok(teams) => teams,
        Err(error) => {
            logging::verbose(
                "[Accio]",
                &format!("团队列表查询失败（不影响添加）: {}", error.message),
            );
            return;
        }
    };
    let active: Vec<&Value> = teams
        .iter()
        .filter(|team| {
            let team_id = credentials::text(team, &["teamId", "team_id"]);
            let status = credentials::text(team, &["status"]).to_ascii_uppercase();
            !team_id.is_empty() && (status.is_empty() || status == "ACTIVE")
        })
        .collect();
    if active.len() != 1 {
        logging::verbose(
            "[Accio]",
            &format!(
                "账号属于 {} 个可用团队，空间留给用户显式指定（当前按个人空间）",
                active.len()
            ),
        );
        return;
    }
    let team = active[0];
    let team_id = credentials::text(team, &["teamId", "team_id"]);
    let team_name = credentials::text(team, &["teamName", "team_name"]);
    if team_id.is_empty() {
        return;
    }
    credentials.team_id = team_id.chars().take(64).collect();
    credentials.team_name = team_name.chars().take(100).collect();
    logging::log(
        "[Accio]",
        &format!(
            "🔗 账号只属于一个公司空间，已自动绑定：{}（{}）",
            credentials.team_name, credentials.team_id
        ),
    );
}

/// 「填写凭证」路径的入口：解析 payload → 补资料 → 补齐设备指纹。
///
/// ── 为什么要查一次资料（多一次往返）────────────────────────
/// token 是不透明串，账号列表上总得显示「这是谁」；查一次资料就有了 userId /
/// 昵称 / 邮箱三样。查询失败**不算失败**：拿不到资料照常建账号（id 走凭证
/// 指纹兜底、名字走备注名），只是少一行副标题 —— 与 AutoClaw 的
/// `email_or_empty` 同一取舍。
pub async fn prepare_account(payload: &Value) -> Result<Credentials, GatewayError> {
    let mut credentials = Credentials::from_payload(payload)?;
    credentials.ensure_identity();
    if credentials.user_id.is_empty() || credentials.name.is_empty() {
        // 先取结果再 match：`fetch_profile` 借用 credentials，而在分支里要可变借用它
        let profile = fetch_profile(&credentials, None).await;
        match profile {
            Ok(profile) => apply_profile(&mut credentials, &profile),
            Err(error) => logging::verbose(
                "[Accio]",
                &format!("用户资料查询失败（不影响添加）: {}", error.message),
            ),
        }
    }
    // 空间：资料里没带「当前空间」（多数账号是 `activeTeam: null`）时，
    // 按团队列表替它认一次 —— 只有「恰好一个 ACTIVE 团队」才采纳，理由见
    // `adopt_single_team` 的文档。
    adopt_single_team(&mut credentials, None).await;
    credentials.ensure_identity();
    Ok(credentials)
}

/// 额度查询用的「只读快照」：不触发续期（余额是展示动作，烧一次 refreshToken
/// 不划算 —— 过期就让上游回 401，由调用方的刷新重试那条既有链路处置）。
pub fn snapshot(record: &Value) -> Result<Credentials, GatewayError> {
    let mut credentials = Credentials::from_payload(record)?;
    credentials.ensure_identity();
    Ok(credentials)
}

/// 一个便于日志的账号摘要（绝不回显 token）
pub fn describe(credentials: &Credentials) -> String {
    let who = if !credentials.user_id.is_empty() {
        credentials.user_id.clone()
    } else if !credentials.email.is_empty() {
        credentials.email.clone()
    } else {
        "未知账号".to_string()
    };
    format!("{}（{}）", who, credentials.region.label())
}

/// 空 body 的占位（部分接口要求 POST 但不需要体）
pub fn empty_body() -> Value {
    json!({})
}
