use axum::extract::{Query, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use super::super::app_state::AppState;

/// 이벤트 조회 쿼리 파라미터
#[derive(Deserialize)]
pub struct EventQuery {
    /// 카테고리 필터 (strategy, order, position, system)
    pub category: Option<String>,
    /// 종목코드 필터
    pub stock_code: Option<String>,
    /// 날짜 필터 (YYYY-MM-DD)
    pub date: Option<String>,
    /// 조회 건수 (기본 50, 최대 200)
    pub limit: Option<i64>,
}

/// 이벤트 응답 항목
#[derive(Serialize, sqlx::FromRow)]
pub struct EventRow {
    pub id: i64,
    pub event_time: chrono::DateTime<chrono::Utc>,
    pub stock_code: String,
    pub category: String,
    pub event_type: String,
    pub severity: String,
    pub message: String,
    pub metadata: serde_json::Value,
}

/// 일일 결산 조회 쿼리
#[derive(Deserialize)]
pub struct DailyReportQuery {
    /// 시작 날짜 (YYYY-MM-DD, 기본 오늘)
    pub from: Option<String>,
    /// 종료 날짜 (YYYY-MM-DD, 기본 오늘)
    pub to: Option<String>,
}

/// 일일 결산 응답
#[derive(Serialize, sqlx::FromRow)]
pub struct DailyReportRow {
    pub date: chrono::NaiveDate,
    pub stock_code: String,
    pub total_trades: i32,
    pub wins: i32,
    pub losses: i32,
    pub win_rate: f64,
    pub total_pnl_pct: f64,
    pub max_loss_pnl_pct: f64,
    pub avg_entry_slippage: i64,
    pub ws_reconnect_count: i32,
    pub api_error_count: i32,
}

/// 시스템 헬스 응답
#[derive(Serialize)]
pub struct HealthResponse {
    pub db_connected: bool,
    pub active_runners: Vec<String>,
    pub event_logger_fail_count: u64,
    pub today_event_summary: EventSummary,
    /// 2026-04-16 Task 2: WS 상태 스냅샷 (tick/message age, retry_count, terminated).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ws: Option<crate::infrastructure::websocket::connection::WsHealthSnapshot>,
    /// WS tick 경과 시간이 임계 초과인지 여부. UI 경고 표시용.
    pub ws_tick_stale: bool,
}

#[derive(Serialize)]
pub struct EventSummary {
    pub ws_reconnect: i64,
    pub api_error: i64,
    pub ws_tick_gap: i64,
    pub entry_executed: i64,
    pub exit_tp: i64,
    pub exit_market: i64,
}

/// GET /api/v1/monitoring/events — 이벤트 로그 조회
async fn get_events(
    State(state): State<AppState>,
    Query(q): Query<EventQuery>,
) -> Json<Vec<EventRow>> {
    let Some(ref pool) = state.db_pool else {
        return Json(vec![]);
    };

    let limit = q.limit.unwrap_or(50).min(200);
    let date_filter = q.date.as_deref().unwrap_or("");

    let rows: Vec<EventRow> = if !date_filter.is_empty() {
        let date = match chrono::NaiveDate::parse_from_str(date_filter, "%Y-%m-%d") {
            Ok(d) => d,
            Err(_) => return Json(vec![]),
        };
        sqlx::query_as::<_, EventRow>(
            "SELECT id, event_time, stock_code, category, event_type, severity, message, metadata
             FROM event_log
             WHERE event_time::date = $1
             AND ($2 = '' OR category = $2)
             AND ($3 = '' OR stock_code = $3)
             ORDER BY event_time DESC LIMIT $4",
        )
        .bind(date)
        .bind(q.category.as_deref().unwrap_or(""))
        .bind(q.stock_code.as_deref().unwrap_or(""))
        .bind(limit)
        .fetch_all(pool)
        .await
        .unwrap_or_default()
    } else {
        sqlx::query_as::<_, EventRow>(
            "SELECT id, event_time, stock_code, category, event_type, severity, message, metadata
             FROM event_log
             WHERE ($1 = '' OR category = $1)
             AND ($2 = '' OR stock_code = $2)
             ORDER BY event_time DESC LIMIT $3",
        )
        .bind(q.category.as_deref().unwrap_or(""))
        .bind(q.stock_code.as_deref().unwrap_or(""))
        .bind(limit)
        .fetch_all(pool)
        .await
        .unwrap_or_default()
    };

    Json(rows)
}

/// GET /api/v1/monitoring/health — 시스템 헬스 스냅샷
async fn get_health(State(state): State<AppState>) -> Json<HealthResponse> {
    let db_connected = state.db_pool.is_some();

    // 활성 러너 목록
    let active_runners: Vec<String> = {
        let statuses = state.strategy_manager.statuses.read().await;
        statuses
            .values()
            .filter(|s| s.active)
            .map(|s| s.code.clone())
            .collect()
    };

    // EventLogger 실패 카운터
    let event_logger_fail_count = state
        .strategy_manager
        .event_logger
        .as_ref()
        .map(|el| el.fail_count())
        .unwrap_or(0);

    // 오늘 이벤트 요약
    let today_event_summary = if let Some(ref pool) = state.db_pool {
        let today = chrono::Local::now().date_naive();
        let counts: Vec<(String, i64)> = sqlx::query_as(
            "SELECT event_type, COUNT(*)::BIGINT FROM event_log
             WHERE event_time::date = $1
             GROUP BY event_type",
        )
        .bind(today)
        .fetch_all(pool)
        .await
        .unwrap_or_default();

        let get = |t: &str| {
            counts
                .iter()
                .find(|(k, _)| k == t)
                .map(|(_, v)| *v)
                .unwrap_or(0)
        };
        EventSummary {
            ws_reconnect: get("ws_reconnect"),
            api_error: get("api_error"),
            ws_tick_gap: get("ws_tick_gap"),
            entry_executed: get("entry_executed"),
            exit_tp: get("exit_tp"),
            exit_market: get("exit_market"),
        }
    } else {
        EventSummary {
            ws_reconnect: 0,
            api_error: 0,
            ws_tick_gap: 0,
            entry_executed: 0,
            exit_tp: 0,
            exit_market: 0,
        }
    };

    // WS 상태 스냅샷. AppState 에 ws_client 가 주입된 경우에만 채워진다.
    // tick 임계는 기본 120초 — `KIS_WS_STALE_SECS` 환경변수와 동일한 판정이나
    // watchdog 이 장중에만 fatal 을 띄우므로 여기서도 장외는 stale 로 보지 않는다.
    let (ws_snapshot, ws_tick_stale) = if let Some(ref ws) = state.ws_client {
        let snap = ws.health_snapshot().await;
        let now_t = chrono::Local::now().time();
        let in_market = now_t >= chrono::NaiveTime::from_hms_opt(9, 0, 0).unwrap()
            && now_t <= chrono::NaiveTime::from_hms_opt(15, 35, 0).unwrap();
        let threshold = std::env::var("KIS_WS_STALE_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(120);
        let tick_stale = match snap.tick_age_secs() {
            Some(age) => in_market && age > threshold,
            None => {
                in_market
                    && (now_t - chrono::NaiveTime::from_hms_opt(9, 10, 0).unwrap()).num_seconds()
                        > 0
            }
        };
        (Some(snap), tick_stale)
    } else {
        (None, false)
    };

    Json(HealthResponse {
        db_connected,
        active_runners,
        event_logger_fail_count,
        today_event_summary,
        ws: ws_snapshot,
        ws_tick_stale,
    })
}

/// GET /api/v1/monitoring/daily-report — 일일 결산 조회
async fn get_daily_report(
    State(state): State<AppState>,
    Query(q): Query<DailyReportQuery>,
) -> Json<Vec<DailyReportRow>> {
    let Some(ref pool) = state.db_pool else {
        return Json(vec![]);
    };

    let today = chrono::Local::now().date_naive();
    let from = q
        .from
        .as_deref()
        .and_then(|s| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
        .unwrap_or(today);
    let to =
        q.to.as_deref()
            .and_then(|s| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
            .unwrap_or(today);

    let rows: Vec<DailyReportRow> = sqlx::query_as(
        "SELECT date, stock_code, total_trades, wins, losses, win_rate, total_pnl_pct,
                max_loss_pnl_pct, avg_entry_slippage, ws_reconnect_count, api_error_count
         FROM daily_report
         WHERE date >= $1 AND date <= $2
         ORDER BY date DESC, stock_code",
    )
    .bind(from)
    .bind(to)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    Json(rows)
}

/// 2026-04-19 PR #2.e: 전체 러너의 armed watch 지표.
///
/// `ArmedWatchStats` (이유별 카운터 + duration + outcome별 평균) 를 그대로 노출.
/// 운영 대시보드에서 drift_exceeded / preflight_failed / cutoff / stopped 빈도와
/// ready/aborted 평균 지연을 즉시 확인할 수 있다.
async fn get_armed_stats(
    State(state): State<AppState>,
) -> Json<Vec<crate::presentation::routes::strategy::ArmedStatsEntry>> {
    Json(state.strategy_manager.snapshot_armed_stats().await)
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/monitoring/events", get(get_events))
        .route("/api/v1/monitoring/health", get(get_health))
        .route("/api/v1/monitoring/daily-report", get(get_daily_report))
        .route("/api/v1/monitoring/armed-stats", get(get_armed_stats))
}
