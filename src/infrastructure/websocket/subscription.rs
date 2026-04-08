use async_trait::async_trait;
use std::collections::{HashMap, HashSet};
use tokio::sync::RwLock;

use crate::domain::error::KisError;
use crate::domain::ports::realtime::{RealtimePort, SubscriptionType};
use crate::domain::types::StockCode;

/// WebSocket 구독 관리자
pub struct SubscriptionManager {
    /// tr_id → 구독 중인 종목코드 집합
    subscriptions: RwLock<HashMap<String, HashSet<String>>>,
}

impl Default for SubscriptionManager {
    fn default() -> Self {
        Self::new()
    }
}

impl SubscriptionManager {
    pub fn new() -> Self {
        Self {
            subscriptions: RwLock::new(HashMap::new()),
        }
    }

    /// 구독 추가
    pub async fn add(&self, tr_id: &str, stock_code: &str) {
        let mut subs = self.subscriptions.write().await;
        subs.entry(tr_id.to_string())
            .or_default()
            .insert(stock_code.to_string());
    }

    /// 구독 제거
    pub async fn remove(&self, tr_id: &str, stock_code: &str) {
        let mut subs = self.subscriptions.write().await;
        if let Some(codes) = subs.get_mut(tr_id) {
            codes.remove(stock_code);
            if codes.is_empty() {
                subs.remove(tr_id);
            }
        }
    }

    /// 재연결 시 구독 복원 메시지 생성
    pub fn restore_messages(&self, approval_key: &str) -> Vec<String> {
        // 동기 접근을 위해 try_read 사용
        let subs = match self.subscriptions.try_read() {
            Ok(guard) => guard,
            Err(_) => return vec![],
        };

        let mut messages = Vec::new();
        for (tr_id, codes) in subs.iter() {
            for code in codes {
                messages.push(build_subscribe_message(approval_key, tr_id, code));
            }
        }
        messages
    }

    fn sub_type_to_tr_id(sub_type: &SubscriptionType) -> &'static str {
        match sub_type {
            SubscriptionType::Execution => "H0STCNT0",
            SubscriptionType::OrderBook => "H0STASP0",
        }
    }
}

#[async_trait]
impl RealtimePort for SubscriptionManager {
    async fn subscribe(
        &self,
        stock_code: &StockCode,
        sub_type: SubscriptionType,
    ) -> Result<(), KisError> {
        let tr_id = Self::sub_type_to_tr_id(&sub_type);
        self.add(tr_id, stock_code.as_str()).await;
        Ok(())
    }

    async fn unsubscribe(
        &self,
        stock_code: &StockCode,
        sub_type: SubscriptionType,
    ) -> Result<(), KisError> {
        let tr_id = Self::sub_type_to_tr_id(&sub_type);
        self.remove(tr_id, stock_code.as_str()).await;
        Ok(())
    }
}

/// KIS WebSocket 구독 요청 JSON 생성
fn build_subscribe_message(approval_key: &str, tr_id: &str, tr_key: &str) -> String {
    serde_json::json!({
        "header": {
            "approval_key": approval_key,
            "custtype": "P",
            "tr_type": "1",
            "content-type": "utf-8"
        },
        "body": {
            "input": {
                "tr_id": tr_id,
                "tr_key": tr_key
            }
        }
    })
    .to_string()
}

/// 구독 해제 요청 JSON
pub fn build_unsubscribe_message(approval_key: &str, tr_id: &str, tr_key: &str) -> String {
    serde_json::json!({
        "header": {
            "approval_key": approval_key,
            "custtype": "P",
            "tr_type": "2",
            "content-type": "utf-8"
        },
        "body": {
            "input": {
                "tr_id": tr_id,
                "tr_key": tr_key
            }
        }
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_subscription_add_remove() {
        let mgr = SubscriptionManager::new();
        mgr.add("H0STCNT0", "005930").await;
        mgr.add("H0STCNT0", "000660").await;

        let messages = mgr.restore_messages("test_key");
        assert_eq!(messages.len(), 2);

        mgr.remove("H0STCNT0", "005930").await;
        let messages = mgr.restore_messages("test_key");
        assert_eq!(messages.len(), 1);
    }
}
