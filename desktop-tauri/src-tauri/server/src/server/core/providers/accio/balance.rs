//! Accio 额度查询：归一成账号页的统一形状（`ProviderAdapter::query_usage` 契约）。
//!
//! ── 上游给的是什么 ──────────────────────────────────────────
//! `GET /api/entitlement/quota?accessToken=…` → `{usagePercent, refreshCountdownSeconds}`
//! （桌面端本地路由 `/auth/quota` 就是把它算成这两个数给界面用的）。
//! 另有 `GET /api/entitlement/currentSubscription?accessToken=…` 给订阅详情
//! （套餐名 / 到期时间 / 剩余额度），它可能不存在（免费账号），失败就当没有。
//!
//! ── 积分到底怎么读（2026-10 实测，结论与最初的猜测不同）────────
//! `currentSubscription` 的原文是**按池分组**的（真实抓到的两个样本）：
//!
//! ```jsonc
//! // 个人（免费）账号
//! { "type": "FREE", "currentPlanId": "101", "total": 20, "remaining": 20,
//!   "entitlement": { "daily":    { "total": 20, "remaining": 20, "used": 0 },
//!                    "monthly":  { "total": 0,  "remaining": 0 },
//!                    "referral": { "total": 0,  "remaining": 0 } } }
//!
//! // 团队空间（同一端点、同一 token，桌面端在团队空间下拿到的）
//! { "type": "TEAM", "currentPlanId": "503", "planName": "Team Standard",
//!   "total": 2000, "remaining": 1999,
//!   "entitlement": { "monthly": { "total": 2000, "used": 1, "remaining": 1999 },
//!                    "daily": null, "referral": null } }
//! ```
//!
//! 所以「剩余多少积分」是**有绝对值的**，就在 `entitlement.<池>` 里（顶层
//! `total/remaining` 是当前生效那个池的镜像）。本模块按池展开，一个池一条
//! wallet，`available` 取各池剩余之和 —— 不再像改造前那样把 `usagePercent`
//! 当唯一读数（那样连 20 还是 2000 都分不出来）。
//!
//! ── 公司空间（团队）的额度为什么**读不到**（2026-10 复核后的定论）──
//! 结论是「空间维度不在额度接口上」，不是「空间没法用」：
//!   - `/api/entitlement/quota` 与 `currentSubscription` 是**账号级**接口：带上
//!     `x-team-id: <teamId>`（实测 `teamHeaderSent: true`）响应**一字不变**，
//!     永远是个人计划那一份（`type: FREE` + `entitlement.daily`）；
//!   - 换成 `teamId=37001` 与本地复合键 `7083531284_37001` 两种取值，同样不变；
//!   - 上游没有 `/api/space/*`、`/api/org/*`、`/api/workspace/*` 这类端点（全 404），
//!     空间切换是**桌面端本地**完成的（本地路由 `POST /space/switch`）。
//!
//! 但空间本身是**可用**的，区别只在「哪些接口认这个头」：
//!   - 推理 `/api/adk/llm/generateContent` **认** —— 带头即消耗公司池
//!     （见 `chat::build_plan`，这是「反代团队空间」的落点）；
//!   - 账单/用量 `/api/entitlement/listUserMessageBill` 与 `queryCreditUsageTrend`
//!     **认** —— 同一 token、同一台机器，带头与不带头是两个空间的账
//!     （实测 104 条 vs 18 条、当日 336 vs 1 积分）。
//!
//! 因此这里**不给额度读数贴「公司空间」标签**：读数来自账号级接口，它就是个人池
//! 的数；按 `teamId` 贴标签会把个人额度冒充成公司额度（改造过程中确实发生过）。
//! 标签只认响应里的 `type`：`TEAM` 才叫公司空间。账号上配了 `teamId` 的，读数
//! 仍是个人池 —— 面板要区分得看账号记录上的 `teamId`，以及账单接口的按空间口径。
//!
//! ── 401 为什么原样透出 ──────────────────────────────────────
//! 契约要求（见 `ProviderAdapter::query_usage`）：调用方据此走「刷新后重试一次」。
//! 注意上游对失效 token 回的是 **HTTP 200 + `{"success":false,"code":"401"}`**，
//! 只看 HTTP 状态会把一次失败读成「余量 100%」—— 业务码判定见 [`auth::business_error`]。

use serde_json::{json, Value};

use crate::server::core::account_store::AccountStore;
use crate::server::errors::GatewayError;

use super::auth;
use super::endpoints;

/// 查一个账号的额度 / 订阅，归一成统一形状。
///
/// `region` 必给：两个地区是两家 provider，取记录时按它过滤。
pub async fn query(
    store: &AccountStore,
    account_id: &str,
    region: super::endpoints::Region,
) -> Result<Value, GatewayError> {
    let record = store
        .accio_account_record(account_id, region.provider_id())
        .ok_or_else(|| GatewayError::with_status(404, "Accio 账号不存在，请先添加账号"))?;
    let credentials = auth::snapshot(&record)?;
    let proxy = auth::account_proxy(&record)?;
    // 空间维度：有 teamId 才带（个人空间不带，口径见 endpoints::push_team_header）
    let team_id = if credentials.is_team() {
        Some(credentials.team_id.as_str())
    } else {
        None
    };

    // ── 额度用量 ────────────────────────────────────────────────
    let quota_response = auth::get_with_token(
        credentials.region,
        endpoints::QUOTA_PATH,
        &credentials.access_token,
        &[],
        team_id,
        proxy.as_ref(),
    )
    .await?;
    if quota_response.status == 401 || quota_response.status == 403 {
        return Err(GatewayError::with_status(401, "Accio 凭证已失效，请重新登录"));
    }
    let quota_payload = auth::payload(quota_response, "额度查询")?;
    // HTTP 200 里的业务失败（最典型的就是 code 401 NOT_LOGIN）
    if let Some((status, message)) = auth::business_error(&quota_payload) {
        return Err(GatewayError::with_status(
            status,
            format!("Accio 额度查询失败：{message}"),
        ));
    }
    let quota = auth::unwrap_data(quota_payload);
    let usage_percent = quota
        .get("usagePercent")
        .or_else(|| quota.get("usage_percent"))
        .and_then(Value::as_f64)
        .unwrap_or(0.0)
        .clamp(0.0, 100.0);
    let countdown = quota
        .get("refreshCountdownSeconds")
        .or_else(|| quota.get("refresh_countdown_seconds"))
        .and_then(Value::as_f64);
    let remain = (100.0 - usage_percent).max(0.0);

    // ── 订阅（可选，失败不影响额度的展示）──────────────────────
    // 原始响应单独留一份：团队额度到底藏在哪个字段（顶层 total/remaining，还是
    // `entitlement.monthly` 这样的嵌套池），只有看原文才能确定 —— 归一化之后
    // 只剩我们猜的那几个键名，猜错就永远读不到。
    let mut subscription_raw = Value::Null;
    let subscription = match auth::get_with_token(
        credentials.region,
        endpoints::SUBSCRIPTION_PATH,
        &credentials.access_token,
        &[],
        team_id,
        proxy.as_ref(),
    )
    .await
    {
        Ok(response) if response.ok => match response.payload {
            // 业务失败（未登录 / 空间无订阅）当「没有」，不给一个空壳订阅
            Some(payload) if auth::business_error(&payload).is_none() => {
                let inner = auth::unwrap_data(payload);
                subscription_raw = inner.clone();
                subscription_shape(&inner)
            }
            _ => Value::Null,
        },
        // 免费账号没有订阅：失败当「没有」，不影响额度那一半的展示
        _ => Value::Null,
    };

    // ── 按池展开：一个池一条 wallet ─────────────────────────────
    // 池来自订阅原文（`entitlement.daily/monthly/referral`）；顶层
    // `total/remaining` 作为兜底（免费账号常常只有顶层那一份）。
    let plan_type = subscription_raw
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("");
    let is_team = plan_type.eq_ignore_ascii_case("TEAM");
    let pools = pools_of(&subscription_raw);

    let mut wallets: Vec<Value> = pools
        .iter()
        .map(|pool| {
            let mut wallet = json!({
                "type": pool.key,
                "displayName": pool.label,
                "balance": pool.remaining,
                "total": pool.total,
                "used": pool.used,
                "unit": "积分",
            });
            if let Some(detail) = &pool.detail {
                wallet["detail"] = Value::String(detail.clone());
            }
            if pool.total > 0.0 {
                wallet["usedPercent"] = json!((pool.used / pool.total * 100.0).clamp(0.0, 100.0));
            }
            wallet
        })
        .collect();

    // 池读不出来时退回百分比（少数账号只有 quota，没有订阅详情）
    if wallets.is_empty() {
        let mut wallet = json!({
            "type": "entitlement",
            "displayName": "额度余量",
            "balance": remain,
            "unit": "%",
            "usedPercent": usage_percent,
        });
        if let Some(seconds) = countdown.filter(|value| *value > 0.0) {
            wallet["detail"] = Value::String(format!("{} 后重置", humanize_seconds(seconds)));
        }
        wallets.push(wallet);
    }

    let available = if pools.is_empty() {
        Value::Null
    } else {
        json!(pools.iter().map(|pool| pool.remaining).sum::<f64>())
    };
    let unit = if pools.is_empty() { "%" } else { "积分" };

    Ok(json!({
        "available": available,
        "unit": unit,
        "wallets": wallets,
        "subscription": subscription,
        // 空间标签**只认响应里的 `type`**（`TEAM` 才是公司空间）。
        // 不看账号上的 teamId —— 那把 token 读不到团队池，按 teamId 贴标签会把
        // 个人额度冒充成公司额度（见模块头）。
        "scope": if is_team { "team" } else { "personal" },
        "planType": if plan_type.is_empty() { Value::Null } else { Value::String(plan_type.to_string()) },
        "teamId": if is_team && !credentials.team_id.is_empty() {
            Value::String(credentials.team_id.clone())
        } else {
            Value::Null
        },
        "teamName": if is_team && !credentials.team_name.is_empty() {
            Value::String(credentials.team_name.clone())
        } else {
            Value::Null
        },
        "raw": {
            "quota": quota,
            "region": credentials.region.edition(),
            "account": credentials.user_id,
            // 账号上配的空间（只作记录；当前 token 读不读得到是另一回事）
            "configuredTeamId": if credentials.is_team() {
                Value::String(credentials.team_id.clone())
            } else {
                Value::Null
            },
            // 本次请求有没有带 x-team-id（排障用：实测带了也不改变响应）
            "teamHeaderSent": team_id.is_some(),
            // 订阅接口原文（未归一化）—— 池的字段名从这里来
            "subscriptionRaw": subscription_raw,
        },
    }))
}

/// 订阅原文里的一个积分池
struct Pool {
    key: &'static str,
    label: &'static str,
    total: f64,
    remaining: f64,
    used: f64,
    detail: Option<String>,
}

/// 从订阅原文取各积分池。
///
/// 顺序固定为 daily → monthly → referral，`total <= 0` 的空池直接丢掉（免费账号
/// 的 monthly/referral 都是 0/0，留着只会让面板多两行「0 积分」）。三个嵌套池
/// 一个都没有时，退回顶层的 `total/remaining` —— 那通常是当前生效的那个池的
/// 镜像，比什么都不显示强。
fn pools_of(subscription: &Value) -> Vec<Pool> {
    const POOLS: [(&str, &str); 3] = [
        ("daily", "每日积分"),
        ("monthly", "月度积分"),
        ("referral", "邀请奖励"),
    ];
    let mut pools = Vec::new();
    for (key, label) in POOLS {
        let Some(node) = subscription
            .get("entitlement")
            .and_then(|entitlement| entitlement.get(key))
            .filter(|value| value.is_object())
        else {
            continue;
        };
        let total = number_at(node, &["total"]);
        if total <= 0.0 {
            continue;
        }
        let remaining = number_at(node, &["remaining", "remain", "left"]);
        let used = number_at(node, &["used"]).max(0.0);
        let detail = node
            .get("nextBillingDate")
            .and_then(Value::as_str)
            .filter(|text| !text.trim().is_empty())
            .map(|text| format!("下次发放 {text}"));
        pools.push(Pool { key, label, total, remaining, used, detail });
    }
    if pools.is_empty() {
        let total = number_at(subscription, &["total", "totalQuota"]);
        if total > 0.0 {
            pools.push(Pool {
                key: "entitlement",
                label: "额度余量",
                total,
                remaining: number_at(subscription, &["remaining", "remainQuota"]),
                used: number_at(subscription, &["used"]),
                detail: None,
            });
        }
    }
    pools
}

/// 取一个数值字段（缺失 / 非数字 → 0）
fn number_at(node: &Value, keys: &[&str]) -> f64 {
    keys.iter()
        .find_map(|key| node.get(*key).and_then(Value::as_f64))
        .unwrap_or(0.0)
}

/// 订阅响应 → 契约里的 `subscription` 形状（字段名宽口径）。
fn subscription_shape(payload: &Value) -> Value {
    let text = |keys: &[&str]| -> Value {
        keys.iter()
            .find_map(|key| payload.get(*key).and_then(Value::as_str))
            .filter(|value| !value.trim().is_empty())
            .map(|value| Value::String(value.to_string()))
            .unwrap_or(Value::Null)
    };
    let number = |keys: &[&str]| -> Value {
        keys.iter()
            .find_map(|key| payload.get(*key).and_then(Value::as_f64))
            .map(|value| json!(value))
            .unwrap_or(Value::Null)
    };
    // `endDate` 是实际字段名（`expireAt` 那一套是早期宽口径留下的，保留兼容）
    let expire = ["endDate", "expireAt", "expiresAt", "endTime", "end_time"]
        .iter()
        .find_map(|key| payload.get(*key))
        .cloned()
        .unwrap_or(Value::Null);
    json!({
        // 套餐类型：FREE / PRO / TEAM …（`TEAM` 才是公司空间）
        "type": text(&["type"]),
        "planName": text(&["planName", "plan_name", "productName", "name"]),
        "status": text(&["status", "state"]),
        "expireAt": expire,
        "startAt": payload
            .get("startDate")
            .cloned()
            .unwrap_or(Value::Null),
        "remainQuota": number(&["remainQuota", "remain", "remaining", "leftQuota"]),
        "totalQuota": number(&["totalQuota", "total", "quota"]),
    })
}

/// 秒 → 「3 小时 20 分」这类人话（界面直接显示）
fn humanize_seconds(seconds: f64) -> String {
    let total = seconds.max(0.0) as i64;
    let days = total / 86_400;
    let hours = (total % 86_400) / 3_600;
    let minutes = (total % 3_600) / 60;
    if days > 0 {
        format!("{days} 天 {hours} 小时")
    } else if hours > 0 {
        format!("{hours} 小时 {minutes} 分")
    } else if minutes > 0 {
        format!("{minutes} 分")
    } else {
        format!("{total} 秒")
    }
}
