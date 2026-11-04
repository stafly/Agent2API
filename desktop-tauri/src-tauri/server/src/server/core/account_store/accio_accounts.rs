//! Accio 账号（两个地区各一家 provider）：添加、凭证刷新回写、公开形态。
//!
//! ── 身份怎么认 ──────────────────────────────────────────────
//! Accio 的 accessToken 是**不透明串**（不是 JWT），解不出 userId，因此身份
//! 由凭证里的 `userId`（网页登录 / 查资料拿到）决定。查不到 userId 时退回
//! **按 accessToken 相等**去重 —— 同一串凭证重复添加命中同一条记录，不会
//! 堆出一串重复账号。
//!
//! ── 两个地区为什么共用这一份实现 ─────────────────────────────
//! 账号字段、续期链路、公开形态两地完全一致，差别只在域名与 `x-package-region`
//! （那是转发与凭证层的事，账号层不体现）。`provider` 字段区分两地：
//! `accio` / `accio-cn`，因此**记录 id 也带地区前缀**，两条记录不会撞 id。

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::server::core::providers::accio::credentials::Credentials;
use crate::server::core::providers::accio::endpoints::Region;
use crate::server::logging;

use super::priority::next_free_priority;
use super::sql;
use super::state::{mark_name_custom, StoredAccount};
use super::store::{AccountStore, AccountStoreError};
use super::store_util::{token_tail_of, truncate_chars};
use super::CredentialWrite;

impl AccountStore {
    /// 取一条 Accio 账号记录。
    ///
    /// `provider_id` 是 `accio` / `accio-cn` 之一 —— **必须给**：两个地区是两家
    /// provider，国际版的凭证打不了国内版的站点（反之亦然），混着挑会让转发
    /// 拿一个必然 401 的账号去试。
    ///
    /// `account_id` 为空时给「这一家的队首可用账号」（与转发默认选路同一口径）。
    pub fn accio_account_record(&self, account_id: &str, provider_id: &str) -> Option<Value> {
        let guard = self.guard();
        if !account_id.is_empty() {
            let record = self.record_by_id(&guard, account_id)?;
            return (record.provider() == provider_id).then(|| record.to_value());
        }
        self.records_for_provider(&guard, provider_id)
            .into_iter()
            .filter(|record| record.enabled() && record.has_token())
            .min_by_key(|record| record.order_key())
            .map(|record| record.to_value())
    }

    /// 添加 / 更新一条 Accio 账号（手动粘贴与网页登录共用这一个入口）。
    pub fn add_accio_account(
        &self,
        credentials: &Credentials,
        name: Option<&str>,
        source: &str,
    ) -> Result<Value, AccountStoreError> {
        let mut credentials = credentials.clone();
        credentials.ensure_identity();
        let region = credentials.region;
        let provider = region.provider_id();
        let guard = self.guard();
        // 去重：同一账号 + **同一空间**才算同一条记录。
        //
        // ── 为什么必须带上空间（2026-10 实测）──────────────────────
        // Accio 的「个人空间 / 公司空间」用的是**同一份 token**（空间只是请求头
        // `x-team-id`，换空间不换票）。因此只按 userId 去重会让两个空间撞成一条：
        // 后添加的公司空间直接覆盖掉个人空间的记录，两个池的额度永远只能看到一个。
        // 同理，只按 accessToken 相等去重也一样 —— 那串凭证本来就同时属于两个空间。
        // 带上 teamId 之后，「同一账号的两个空间」在库里是两条独立账号。
        let existing = self
            .records_for_provider(&guard, provider)
            .into_iter()
            .find(|record| {
                if accio_region_of(record) != Some(region) {
                    return false;
                }
                let same_user = !credentials.user_id.is_empty()
                    && record.user_id() == credentials.user_id
                    && record_team_of(record) == credentials.team_id.trim();
                let same_token = record
                    .get("accessToken")
                    .and_then(Value::as_str)
                    .is_some_and(|value| value == credentials.access_token)
                    && record_team_of(record) == credentials.team_id.trim();
                same_user || same_token
            });
        let id = existing
            .as_ref()
            .map(|record| record.id().to_string())
            .unwrap_or_else(|| {
                let seed = if credentials.user_id.is_empty() {
                    if credentials.is_team() {
                        // 没有 userId 时只能靠凭证指纹；团队维度也必须进种子，
                        // 否则同一串凭证的两个空间会算出同一个 id
                        format!("{}#{}", credentials.access_token, credentials.team_id.trim())
                    } else {
                        credentials.access_token.clone()
                    }
                } else if credentials.is_team() {
                    // 团队空间：在历史形态后再加一段，个人空间的 id 因此逐字不变
                    // （既有账号不会被重新添加时算出一个新 id 而变成重复记录）
                    format!(
                        "{}-{}-{}",
                        region.edition(),
                        credentials.user_id,
                        credentials.team_id.trim()
                    )
                } else {
                    format!("{}-{}", region.edition(), credentials.user_id)
                };
                format!(
                    "accio-{}-{:x}",
                    region.edition(),
                    Sha256::digest(seed.as_bytes())
                )
            });
        if existing.is_none() && self.record_by_id(&guard, &id).is_some() {
            return Err(AccountStoreError::new(
                "Accio 账号 ID 已被其它账号占用，请先核对账号记录",
                409,
            ));
        }
        let record_name = name
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .or_else(|| existing.as_ref().map(StoredAccount::name).filter(|value| !value.is_empty()))
            .unwrap_or_else(|| {
                let base = if !credentials.name.is_empty() {
                    credentials.name.clone()
                } else if !credentials.email.is_empty() {
                    credentials.email.clone()
                } else if !credentials.user_id.is_empty() {
                    format!("Accio {}", credentials.user_id)
                } else {
                    format!("Accio {}账号", region.label())
                };
                // 同一账号的两个空间是两条记录：不标空间名在列表里会长得一模一样，
                // 用户根本分不清哪条是公司池（备注名由用户显式给过时不加后缀）
                if credentials.is_team() {
                    let space = if credentials.team_name.trim().is_empty() {
                        credentials.team_id.trim().to_string()
                    } else {
                        credentials.team_name.trim().to_string()
                    };
                    format!("{base}（{space}）")
                } else {
                    base
                }
            });
        let mut fields = existing
            .as_ref()
            .map(|record| record.fields().clone())
            .unwrap_or_default();
        if let Value::Object(values) = credentials.to_value() {
            for (key, value) in values {
                // 空值不覆盖既有内容（重新登录时只给了一个 accessToken，
                // 不该把上次的 refreshToken / expiresAt 洗掉）
                if value.is_null() {
                    continue;
                }
                if matches!(&value, Value::String(text) if text.is_empty()) {
                    continue;
                }
                fields.insert(key, value);
            }
        }
        let priority = match existing.as_ref() {
            Some(record) => record.priority(),
            None => {
                let used = self
                    .with_conn(&guard, |conn| sql::priorities_all(conn))
                    .unwrap_or_default();
                next_free_priority(&used)
            }
        };
        fields.insert("id".to_string(), Value::String(id.clone()));
        fields.insert("provider".to_string(), Value::String(provider.to_string()));
        fields.insert("name".to_string(), Value::String(truncate_chars(&record_name, 100)));
        mark_name_custom(&mut fields, name.is_some_and(|value| !value.trim().is_empty()), existing.as_ref());
        fields.insert(
            "tokenTail".to_string(),
            Value::String(token_tail_of(&credentials.access_token)),
        );
        fields.insert("priority".to_string(), Value::from(priority));
        fields.insert(
            "enabled".to_string(),
            Value::Bool(existing.as_ref().map(StoredAccount::enabled).unwrap_or(true)),
        );
        fields.insert("desktop".to_string(), Value::Bool(false));
        fields.insert("source".to_string(), Value::String(source.to_string()));
        fields.insert(
            "addedAt".to_string(),
            Value::from(existing.as_ref().map(StoredAccount::added_at).unwrap_or_else(logging::now_ms)),
        );
        fields.insert("updatedAt".to_string(), Value::from(logging::now_ms()));
        fields.insert("rateLimits".to_string(), json!({}));
        for key in ["edition", "endpoint", "prefixPath", "platform", "access", "refresh", "expires", "pat"] {
            fields.remove(key);
        }
        let record = StoredAccount::from_map(fields);
        self.with_conn(&guard, |conn| sql::put(conn, &record))?;
        logging::log(
            "[Accounts]",
            &format!("✅ Accio {}账号已保存（优先级 {priority}）", region.label()),
        );
        Ok(self.public_account(&record))
    }

    /// 给**已有**账号设置 / 清除公司空间标识（`teamId` / `teamName`）。
    ///
    /// ── 为什么需要单独一个入口（而不是让用户重新添加一次）──────────
    /// 添加账号那条路要求手里有 accessToken，而面板只显示 `tokenTail` —— 已经
    /// 加好的账号没法靠「再添加一次」补上 teamId。这里按 id 原地改，凭证一个字
    /// 不动：改的只是「这条记录读哪个空间的额度」。
    ///
    /// 取值语义：`null` 或空串 = 清除（回到个人空间）；其余按 trim 后的文本落盘。
    /// 长度与控制字符做防御性校验（与凭证字段同一口径），避免把脏值落进库。
    pub fn update_accio_team(
        &self,
        account_id: &str,
        team_id: Option<&Value>,
        team_name: Option<&Value>,
    ) -> Result<Vec<String>, AccountStoreError> {
        let guard = self.guard();
        let Some(mut record) = self.record_by_id(&guard, account_id) else {
            return Err(AccountStoreError::new("账号不存在", 404));
        };
        if !super::is_accio_family(&record.provider()) {
            return Err(AccountStoreError::new(
                "只有 Accio 账号有公司空间设置（teamId）",
                400,
            ));
        }
        let mut changes = Vec::new();
        for (key, value, limit) in [
            ("teamId", team_id, 64usize),
            ("teamName", team_name, 100usize),
        ] {
            let Some(value) = value else { continue };
            let text = match value {
                Value::Null => String::new(),
                Value::String(text) => text.trim().to_string(),
                _ => {
                    return Err(AccountStoreError::bad_request(format!(
                        "{key} 必须是字符串或 null"
                    )))
                }
            };
            if text.chars().count() > limit || text.chars().any(char::is_control) {
                return Err(AccountStoreError::bad_request(format!("{key} 过长或含非法字符")));
            }
            // 清空就删键：记录里不留空串，去重与判定都按「有没有这个字段」走
            if text.is_empty() {
                record.fields_mut().remove(key);
            } else {
                record.fields_mut().insert(key.to_string(), Value::String(text));
            }
            changes.push(key.to_string());
        }
        record.set_updated_at(logging::now_ms());
        self.with_conn(&guard, |conn| sql::update_in_place(conn, &record))?;
        Ok(changes)
    }

    /// 刷新结果回写（比较-再写：记录里的凭证仍是刷新前那份才写）
    pub fn update_accio_credentials_if_current(
        &self,
        expected: &Value,
        credentials: &Credentials,
    ) -> Result<CredentialWrite, AccountStoreError> {
        let id = expected.get("id").and_then(Value::as_str).unwrap_or("");
        let guard = self.guard();
        let Some(mut record) = self
            .record_by_id(&guard, id)
            .filter(|record| accio_region_of(record).is_some())
        else {
            return Ok(CredentialWrite::Stale);
        };
        for key in ["accessToken", "refreshToken", "userId", "deviceId", "addedAt"] {
            if record.get(key) != expected.get(key) {
                return Ok(CredentialWrite::Stale);
            }
        }
        if accio_region_of(&record) != Some(credentials.region) {
            return Err(AccountStoreError::bad_request("Accio 刷新结果与原账号地区不一致"));
        }
        if !credentials.user_id.is_empty() && record.user_id() != credentials.user_id {
            return Err(AccountStoreError::bad_request("Accio 刷新结果与原账号身份不一致"));
        }
        if let Value::Object(values) = credentials.to_value() {
            for (key, value) in values {
                if value.is_null() || matches!(&value, Value::String(text) if text.is_empty()) {
                    continue;
                }
                record.fields_mut().insert(key, value);
            }
        }
        record.set("tokenTail", Value::String(token_tail_of(&credentials.access_token)));
        record.set_updated_at(logging::now_ms());
        self.with_conn(&guard, |conn| sql::update_in_place(conn, &record))?;
        Ok(CredentialWrite::Written)
    }

    /// 公开形态（两个地区共用；`edition` / `editionLabel` 体现差异）
    pub fn to_accio_public_account(&self, record: &StoredAccount) -> Value {
        let region = accio_region_of(record);
        let available = record.has_token() && region.is_some();
        let can_refresh = Credentials::from_payload(&record.to_value())
            .map(|credentials| credentials.can_refresh())
            .unwrap_or(false);
        let mut public = Map::new();
        for key in ["id", "provider", "name", "userId", "email", "nickname", "source", "tokenTail", "expiresAt", "teamId", "teamName"] {
            public.insert(key.to_string(), record.get(key).cloned().unwrap_or(Value::Null));
        }
        // 空间标注（前端在账号卡片上区分个人 / 公司空间；空 teamId = 个人空间）
        public.insert(
            "space".to_string(),
            Value::String(if record_team_of(record).is_empty() {
                "personal".to_string()
            } else {
                "team".to_string()
            }),
        );
        public.insert(
            "edition".to_string(),
            region
                .map(|value| Value::String(value.edition().to_string()))
                .unwrap_or(Value::Null),
        );
        public.insert(
            "editionLabel".to_string(),
            region
                .map(|value| Value::String(value.label().to_string()))
                .unwrap_or(Value::Null),
        );
        public.insert("hasRefreshToken".to_string(), Value::Bool(can_refresh));
        public.insert("priority".to_string(), Value::from(record.priority()));
        public.insert("enabled".to_string(), Value::Bool(record.enabled()));
        public.insert("addedAt".to_string(), Value::from(record.added_at()));
        public.insert("updatedAt".to_string(), Value::from(record.updated_at()));
        public.insert(
            "proxy".to_string(),
            crate::server::core::proxies::describe_account_proxy(Some(&record.proxy())),
        );
        public.insert(
            "rateLimits".to_string(),
            record.get("rateLimits").cloned().unwrap_or_else(|| json!({})),
        );
        public.insert("desktop".to_string(), Value::Bool(false));
        public.insert("available".to_string(), Value::Bool(available));
        Value::Object(public)
    }
}

/// 记录属于哪个 Accio 地区（不是 Accio 系 → None）。
///
/// 判据是 `provider` 字段（落盘契约），不是记录里的 `mode` —— 后者是凭证的
/// 冗余信息，改错了不该影响分派。
fn accio_region_of(record: &StoredAccount) -> Option<Region> {
    Region::from_provider_id(&record.provider())
}

/// 记录绑定的公司空间 teamId（个人空间 → 空串）。
///
/// 与 [`accio_region_of`] 同一形态：这是个**身份判据**，去重、ID 生成、展示都按
/// 它走，别处不要再各自拼 `record.get("teamId")`。
fn record_team_of(record: &StoredAccount) -> &str {
    record
        .get("teamId")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("")
}
