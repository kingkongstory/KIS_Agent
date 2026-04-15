# PR1 Codex Review 대응 수정 계획

작성일: 2026-04-15
대상 PR: PR1 (P0 실거래 안정화 핫픽스, 미커밋)
검토: Codex adversarial review (2026-04-15)
배포 목표: 2026-04-16 KST 장 시작 전

## Context

2026-04-15에 PR1 (실거래 안정화 P0 핫픽스) 구현 직후 실행한 Codex adversarial review에서 **3건의 critical/high 이슈**가 발견되었다. 세 건 모두 PR1이 지향한 안전성을 오히려 훼손하거나, gate 자체를 무력화한다.

- **#1 [critical]** WS 체결통보 health gate가 실제로 작동하지 않음 — `is_notification_ready()`는 AES key/iv 수립 여부로 판단하는데, 구독 확인 loop가 H0STCNI9 응답을 먼저 소비하면서 AES key 저장 로직을 실행하지 않음. 결과: **정상 구독도 gate 20초 타임아웃 → 매 부팅마다 degraded 고정**.
- **#2 [high]** Manual start가 health gate 판정 전에 통과 — gate는 tokio::spawn으로 비동기 실행, 최대 20초 대기. 그 사이 UI에서 `/strategy/start` 누르면 `system_degraded` 미설정 상태라 통과. 이후 `mark_degraded()`가 호출돼도 이미 시작된 러너는 stop하지 않음 → **gate가 막으려던 상태 그대로 발생**.
- **#3 [high]** Balance fallback이 live 포지션의 broker-side TP를 제거 — `check_and_restore_from_balance()`가 holdings 확인 전에 `cancel_all_pending_orders("restore_fallback")`을 먼저 호출. DB 연결 실패 + 실제로 정상 포지션 보유 + KIS에 TP 지정가 걸린 상황에서 TP만 취소되고, `restore_position()`이 manual_intervention으로 전환하면서 **보호 없는 live position 방치**.

현재 PR1은 미커밋 상태. 세 건을 수정한 뒤 커밋 → 2026-04-16 장 전 배포하거나, 수정 시간이 부족하면 `KIS_AUTO_START=false` 유지 + 수동 관찰 모드로 내일 장을 건너뛰는 방안을 함께 준비한다.

중요: 본 문서는 **PR1 실행 안전성 복구 계획**이다. 이 수정이 끝나더라도 현재 `orb_fvg` 전략의 기대값/백테스트 정합성 문제가 동시에 해결되는 것은 아니다. 따라서 2026-04-16 운영 기준은 아래처럼 둔다.

- `KIS_AUTO_START=false` 유지
- 수동 시작만 허용
- 가능하면 1종목, 1회 이하로 제한
- 기존 백테스트 수익률은 PR1 승인 기준이 아니라 참고 자료로만 사용

이후 PR2 (`orb_vwap_pullback`) / PR3 (ExecutionPolicy) / PR4 (리플레이) 일정은 본 수정 완료 후로 밀린다.

## 이슈별 상세 분석 및 수정 설계

### Fix-1: WS 체결통보 AES key capture 경로 수정 (Codex #1, critical)

#### 근본 원인

`src/infrastructure/websocket/connection.rs`의 메시지 흐름:

1. **line 144-205**: 초기 구독 확인 loop
   - `restore_messages(approval_key)`가 H0STCNT0/ASP0/MKO0/CNI9 구독 메시지를 송신
   - 응답을 받으면 `parse_ws_message` → `WsMessage::Control(ctrl)` 분기에서 `confirmed += 1`만 증가
   - **H0STCNI9/H0STCNI0 응답의 `output.key`/`output.iv` 추출 로직이 없음**
2. **line 230-294**: `handle_message()` 메인 루프 핸들러
   - 동일한 `parse_ws_message` + `WsMessage::Control(ctrl)` 분기에서 line 244-255가 AES key/iv 추출 + `aes_key.write()`

KIS 서버는 구독 응답을 즉시 반환한다. 초기 loop의 5초 deadline 안에 H0STCNI9 응답이 도착할 확률이 매우 높고, 도착하면 그 메시지는 loop에서 소비되어 메인 루프로 넘어가지 않는다. 따라서 **AES key는 영구적으로 저장되지 않음** → `is_notification_ready()`는 항상 false → main.rs health gate가 20초 대기 후 `mark_degraded()`.

#### 수정 설계

`handle_message()`의 Control 처리 로직을 `handle_control_message(ctrl: ControlMessage)` 프라이빗 헬퍼로 분리하고, 초기 loop와 메인 루프 양쪽에서 호출한다.

```rust
// connection.rs
impl KisWebSocketClient {
    /// Control 프레임 처리 공통 경로.
    /// 구독 응답 확인 loop 와 main handle_message 에서 동일하게 호출되어,
    /// H0STCNI9/H0STCNI0 의 AES key/iv 추출이 어느 경로에서 수신되어도 일관되게 수행된다.
    fn handle_control_message(&self, ctrl: &ControlMessage) {
        if ctrl.body.rt_cd == "0" {
            if ctrl.header.tr_id == "H0STCNI9" || ctrl.header.tr_id == "H0STCNI0" {
                if let Some(output) = &ctrl.body.output {
                    let key = output.get("key").and_then(|v| v.as_str()).map(String::from);
                    let iv = output.get("iv").and_then(|v| v.as_str()).map(String::from);
                    if key.is_some() && iv.is_some() {
                        let aes_key = Arc::clone(&self.aes_key);
                        let aes_iv = Arc::clone(&self.aes_iv);
                        tokio::spawn(async move {
                            *aes_key.write().await = key;
                            *aes_iv.write().await = iv;
                        });
                        info!("체결통보 AES key/iv 수신 완료");
                    }
                }
            }
        }
    }
}
```

호출 지점 두 곳 모두 `self.handle_control_message(&ctrl)`로 치환.

#### 검증

- 단위 테스트: ControlMessage(rt_cd=0, tr_id=H0STCNI9, output={key, iv}) 를 넘겨 AES key/iv 가 저장되는지 확인 (기존 구조 때문에 어려우면 수동 테스트).
- 수동 테스트: 서버 기동 → 로그에서 `체결통보 AES key/iv 수신 완료` 가 20초 이내 출력 → `[health gate] 체결통보 준비 완료` 로그 확인.
- 회귀: 메인 루프에서 재연결 후 재구독 시에도 정상 작동. `handle_message`가 아직 있으면서 동일 헬퍼를 호출하므로 재연결 AES key 갱신도 유지.

### Fix-2: Startup notification health 3-state + Pending race 차단 (Codex #2, high)

#### 근본 원인

- `main.rs`의 auto_start 스레드가 `ws_client_for_gate.is_notification_ready()`를 2초 간격 10회 폴링. 20초 동안 `mark_degraded()` 호출 전.
- `strategy.rs:start_runner`는 `self.degraded_reason().await`가 `None`이면 통과 → health 판정 **전**에는 항상 통과.
- `mark_degraded()`는 `system_degraded` + 각 StrategyStatus 에 플래그만 기록하고 `self.runners`의 기존 러너는 건드리지 않음.

결과:
- 시나리오 A: 서버 기동 직후 0-20초 사이 UI에서 수동 start 클릭 → 러너 시작됨. health gate 실패 확정 후에도 그 러너는 계속 돌아감.
- 시나리오 B: health gate는 성공했지만 이후 AES key가 소실된 경우(재연결 실패 등)에 대한 방어 없음. 다만 **이 사후 장애를 startup gate 실패와 같은 상태 전이로 처리하면, 이미 보유한 포지션을 러너 stop → 시장가 청산으로 몰 수 있으므로 PR1 범위에서는 분리**해야 한다.

#### 수정 설계

**중요한 범위 재정의**

PR1의 `NotificationHealth`는 **startup gate 전용**으로 사용한다. 즉:

- `Pending`/`Ready`/`Failed`는 서버 기동 직후 health gate 판정에만 사용
- `Ready` 이후의 WS 재연결 실패/AES key 상실은 **같은 enum으로 재강등하지 않음**
- runtime degradation은 future work로 분리하고, PR1에서는 "신규 진입 차단/운영 경고"까지만 고려

이렇게 분리해야 health 문제를 이유로 이미 열린 live position을 무차별 시장가 청산하는 회귀를 피할 수 있다.

**NotificationHealth enum + 전이 규칙**

```rust
// presentation/routes/strategy.rs 또는 별도 모듈
#[derive(Debug, Clone, PartialEq)]
pub enum NotificationHealth {
    /// 초기 상태 — health gate 판정 이전. start_runner 거부.
    Pending,
    /// AES key/iv 수립 확인 — 거래 허용.
    Ready,
    /// startup gate 실패 — 신규 start 거부.
    Failed(String),
}
```

**StrategyManager 필드 전환**

기존 `system_degraded: Arc<RwLock<Option<String>>>` 를 `notification_health: Arc<RwLock<NotificationHealth>>`로 교체. 초기값 `Pending`.

**start_runner 정책**

```rust
async fn start_runner(&self, code: &str, name: &str) -> Result<(), String> {
    match &*self.notification_health.read().await {
        NotificationHealth::Pending => return Err("체결통보 health check 진행 중 — 잠시 후 재시도".into()),
        NotificationHealth::Failed(reason) => return Err(format!("체결통보 불가 — 거래 차단: {}", reason)),
        NotificationHealth::Ready => { /* 통과 */ }
    }
    // 기존 로직
}
```

**전이 메서드**

```rust
pub async fn mark_notification_ready(&self) {
    *self.notification_health.write().await = NotificationHealth::Ready;
    // 각 StrategyStatus 의 degraded 플래그 해제
    let mut s = self.statuses.write().await;
    for status in s.values_mut() {
        status.degraded = false;
        status.degraded_reason = None;
    }
}

pub async fn mark_notification_startup_failed(&self, reason: String) {
    *self.notification_health.write().await = NotificationHealth::Failed(reason.clone());
    // Pending race로 이미 통과한 러너가 있으면 startup 실패 시점에 한해 강제 stop
    let runners = self.runners.read().await;
    for (code, handle) in runners.iter() {
        warn!("{}: notification_startup_failed — 러너 강제 중단", code);
        handle.stop_flag.store(true, Ordering::Relaxed);
        handle.stop_notify.notify_waiters();
    }
    drop(runners);
    // 각 StrategyStatus 에 degraded 전파
    let mut s = self.statuses.write().await;
    for status in s.values_mut() {
        status.degraded = true;
        status.degraded_reason = Some(reason.clone());
        status.message = format!("degraded: {}", reason);
    }
    // event_log
    if let Some(ref el) = self.event_logger {
        el.log_event("", "system", "notification_startup_failed", "critical", &reason,
            serde_json::json!({}));
    }
}
```

호출 제약:

- `mark_notification_startup_failed`는 `Pending` 상태에서만 호출
- `Ready` 이후 runtime 장애에는 호출하지 않음
- runtime 장애 처리는 본 PR 범위에서 "운영 경고 + 신규 진입 차단"까지만 future work로 남김

**main.rs 수정**

```rust
// 기존 tokio::spawn(async move { ... mgr.auto_start_all() ... }) 블록에서
// mark_degraded 대신 mark_notification_startup_failed, 성공 시 mark_notification_ready 호출
// 그리고 20 초 대신 KIS 응답 여유를 고려해 30 초까지 대기.

tokio::spawn(async move {
    // ... 폴링 ...
    if !notification_ready {
        mgr.mark_notification_startup_failed(
            "KIS_HTS_ID 미설정 또는 AES key/iv 미수립".into()
        ).await;
        error!("[health gate] 자동매매 시작 차단 — Failed 고정");
        return;
    }
    mgr.mark_notification_ready().await;
    info!("[health gate] Notification Ready");
    // 자동 시작은 여전히 KIS_AUTO_START=true 일 때만
    let auto_start = std::env::var("KIS_AUTO_START")...;
    if auto_start { mgr.auto_start_all().await; }
});
```

**의문: auto_start_all 내부의 start_runner 호출**

`auto_start_all`은 여러 종목에 대해 `start_runner`를 순회 호출. `mark_notification_ready` 이후 호출되므로 Pending 거부는 발생하지 않음. 다만 race(매우 짧은 순간)에 대비해 auto_start_all 시작부에서도 Ready 확인 추가.

#### 이전 API 호환성

- `mark_degraded(tag, reason)` 과 `degraded_reason()` 은 main.rs 에서 호출됨. 기능 교체 후 `mark_degraded`는 제거하거나 `mark_notification_startup_failed`로 위임.
- 외부에서 `system_degraded` 상태를 관찰하는 코드는 없음 (routes handler에서 statuses.degraded 를 읽는 것이 전부).

추가 보완:

- `status.degraded` 는 startup gate 실패 시의 시스템 차단 상태와, 러너 자체의 `manual_intervention_required` / `degraded` 상태를 섞어 쓰지 않도록 메시지 우선순위를 정한다.
- `start_runner` 는 `notification_health` 외에도 대상 종목 `status.manual_intervention_required == true` 이면 거부해야 한다. 숨은 live position이 있는 상태에서 같은 종목을 재시작하면 안 된다.

#### 검증

- 단위 테스트: StrategyManager 생성 직후 start_runner 호출 → Err("health check 진행 중"). mark_notification_ready 호출 후 다시 start_runner → Ok.
- 수동 테스트:
  - KIS_HTS_ID 미설정 → 20-30초 후 `[health gate] Failed` 로그 + UI에서 모든 종목이 degraded 메시지로 표시 + start 버튼 눌러도 실패.
  - 정상 기동 후 수동 start → 정상 시작.
  - 기동 직후 0-5초 사이에 UI start 시도 → 거부 메시지.

### Fix-3: Balance-first orphan cleanup + live TP 보존 (Codex #3, high)

#### 근본 원인

`src/strategy/live_runner.rs`의 재시작 복구 두 경로:

1. **`check_and_restore_position()`의 `Ok(None)` 분기** (DB에는 활성 포지션 없음): `cancel_all_pending_orders("orphan_cleanup")` 즉시 호출.
2. **`check_and_restore_from_balance()`** (DB 연결 실패 fallback): `cancel_all_pending_orders("restore_fallback")` 후 `fetch_holding_qty` → 보유 있으면 `restore_position`.

두 경로 모두 **cancel 먼저, holdings 나중** 순서라 다음 시나리오에서 live 포지션의 보호를 스스로 제거한다:

- DB save 실패 후 재시작 (예: active_positions 저장 실패 + 프로세스 크래시) → DB 에는 포지션 없음 → `Ok(None)` 분기 진입 → 동일 종목의 KIS-side TP 지정가까지 함께 취소 → 그 뒤 balance 체크 없이 종료
- DB 연결 자체가 실패 → fallback 경로 진입 → TP 취소 → restore_position 은 manual_intervention + 러너 중단만 수행 → 포지션만 남고 보호 없음

Codex 권고: "Check holdings before canceling pending orders, and if a live position exists preserve or immediately recreate its protective orders before entering manual-intervention mode."

#### 수정 설계

**핵심 규칙**
1. orphan cleanup 전에 **반드시 holdings 조회**.
2. holdings 판단 기준은 **`qty > 0`** 이다. 평균단가(`avg`)가 0/미제공이어도 보유로 간주한다.
3. 보유 있음 → cancel 스킵 + manual_intervention 전환 + 기존 TP 보존.
4. 보유 없음 → cancel 안전, 기존 orphan 정리 로직 그대로.
5. 조회 실패 → fail-safe: cancel 스킵 + manual_intervention (상태 불명이므로 건드리지 않음).

현재 `fetch_holding_qty()` 는 `qty > 0 && avg > 0` 일 때만 `Some`을 반환하므로, 본 수정에서는 `qty > 0`만으로 보유를 판정하는 별도 helper (`fetch_holding_snapshot` 등)로 확장해야 한다.

**공통 헬퍼 도입**

```rust
// live_runner.rs
impl LiveRunner {
    /// Balance-first orphan cleanup.
    /// 실제 잔고를 먼저 확인하여, 정상 포지션이 있으면 절대 미체결 주문을 건드리지 않는다.
    /// 2026-04-15 Codex review #3 대응.
    async fn safe_orphan_cleanup(&self, label: &str) {
        match self.fetch_holding_snapshot().await {
            Ok(None) => {
                // 잔고 없음 — orphan 취소 안전
                if let Err(e) = self.cancel_all_pending_orders(label).await {
                    warn!("{}: [{}] cancel_all 실패: {e}", self.stock_name, label);
                }
            }
            Ok(Some((qty, avg_opt))) => {
                // 잔고 있음 — 기존 TP 보존 + manual_intervention
                warn!(
                    "{}: [{}] 잔고 {}주 감지 — 기존 미체결 주문 보존 + 수동 개입 전환",
                    self.stock_name, label, qty
                );
                self.trigger_manual_intervention(
                    format!(
                        "재시작 복구: 잔고 {}주{} — DB 메타 부재. KIS 기존 TP 확인 필수.",
                        qty,
                        avg_opt.map(|avg| format!(" @ {}원", avg)).unwrap_or_default()
                    ),
                    serde_json::json!({
                        "label": label,
                        "quantity": qty,
                        "avg_price": avg_opt,
                        "tp_cancelled": false,
                    }),
                    ManualInterventionMode::KeepLock,
                ).await;
            }
            Err(e) => {
                // 상태 불명 — fail-safe: 건드리지 않음
                error!(
                    "{}: [{}] balance 조회 실패 — orphan cleanup 스킵, 수동 개입 전환: {e}",
                    self.stock_name, label
                );
                self.trigger_manual_intervention(
                    format!("재시작 복구: balance 조회 실패 ({})", e),
                    serde_json::json!({"label": label, "error": e.to_string()}),
                    ManualInterventionMode::KeepLock,
                ).await;
            }
        }
    }

    /// manual_intervention_required 공통 설정 경로.
    /// state / event_log / stop_flag / stop_notify 를 한 번에 처리.
    /// mode=KeepLock 이면 shared position lock 을 해제하지 않는다.
    async fn trigger_manual_intervention(
        &self,
        reason: String,
        metadata: serde_json::Value,
        mode: ManualInterventionMode,
    ) {
        error!("{}: 수동 개입 필요 — {}", self.stock_name, reason);
        {
            let mut state = self.state.write().await;
            state.phase = "수동 개입 필요".to_string();
            state.manual_intervention_required = true;
            state.degraded = true;
            state.degraded_reason = Some(reason.clone());
        }
        if let Some(ref el) = self.event_logger {
            el.log_event(
                self.stock_code.as_str(),
                "position",
                "manual_intervention_required",
                "critical",
                &reason,
                metadata,
            );
        }
        if matches!(mode, ManualInterventionMode::KeepLock) {
            if let Some(ref lock) = self.position_lock {
                *lock.write().await = PositionLockState::Held(self.stock_code.as_str().to_string());
            }
        }
        self.stop_flag.store(true, Ordering::Relaxed);
        self.stop_notify.notify_waiters();
    }
}
```

**호출 지점 전환**

1. `check_and_restore_position()`의 `Ok(None)` 분기:
   - 기존: `self.cancel_all_pending_orders("orphan_cleanup").await;`
   - 변경: `self.safe_orphan_cleanup("orphan_cleanup").await;`

2. `check_and_restore_from_balance()`:
   - 함수 전체를 `self.safe_orphan_cleanup("restore_fallback").await;` 호출로 대체.
   - 기존의 `restore_position(avg, qty)` 흐름은 `safe_orphan_cleanup` 내부 `Ok(Some(_))` 분기가 대신한다.
   - `restore_position` 메서드 자체는 더이상 호출되지 않으므로 삭제해도 되나, 향후 다른 경로에서 재사용 가능성을 고려해 `#[allow(dead_code)]` 로 유지하거나 완전 삭제. 본 계획에서는 **삭제** (쓰이지 않는 임의 로직을 남기면 실수 유발).

3. `execute_entry`의 `cancel_verify_failed` 분기: 이미 비슷한 manual_intervention 로직이 있으므로 `trigger_manual_intervention`으로 치환하여 일관화.

4. `restore_position`이 삭제되므로 현재 "임의 0.3% 리스크 제거" 규칙은 `safe_orphan_cleanup`의 `Ok(Some(_))` 분기가 담당. 의도는 동일하나 **TP 취소 없이** 처리된다는 점만 변경.

#### 검증

- 리플레이 시나리오
  - 시나리오 R1 (DB는 OK, active_position 없음, 잔고 없음): `Ok(None)` 분기 → `safe_orphan_cleanup` → `fetch_holding_qty = None` → `cancel_all` 실행. 기존 동작과 동일.
  - 시나리오 R2 (DB는 OK, active_position 없음, 잔고 있음): **새 동작** → cancel 스킵 + manual_intervention. 기존에는 TP 제거되던 상황이 방어됨.
  - 시나리오 R3 (DB 연결 실패, 잔고 없음): fallback → `cancel_all` 실행.
  - 시나리오 R4 (DB 연결 실패, 잔고 있음, KIS 에 TP 있음): **critical 회귀 방지** → TP 보존. 기존에는 TP 삭제되던 상황.
  - 시나리오 R5 (DB/balance 모두 실패): `trigger_manual_intervention`, 아무것도 취소하지 않음.
  - 시나리오 R6 (qty>0, avg=0): **여전히 보유로 간주** → cancel 스킵 + manual_intervention. avg가 없다고 orphan으로 처리하면 안 됨.
- 빌드: `cargo check` 통과
- 테스트: `cargo test --lib` 86개 그대로 통과
- 수동 회귀: 로컬에서 active_positions 테이블 DELETE + 재시작 (잔고 0 상태) → orphan_cleanup 정상 로그 확인

### Fix-4: Manual intervention 전용 오류 경로 + shared lock 보존 (계획 보강, P0)

#### 근본 원인

현재 `execute_entry`의 `cancel_verify_failed` 분기는 `manual_intervention_required`를 세우고 `Err(KisError::Internal(...))`를 반환한다. 상위 `poll_and_enter()`는 이를 일반 진입 실패로 취급하여 `abort_entry()`를 호출하고, 그 과정에서 `position_lock`을 `Free`로 되돌린다.

즉 다음 시나리오가 가능하다.

1. 주문 취소 후 balance 조회 실패 → 실제 보유 여부 불명
2. runner는 manual 상태로 바뀌지만, 상위에서 `abort_entry()`가 실행돼 shared lock 해제
3. 다른 종목 runner가 새 진입 시도
4. 실제로는 숨은 live position이 남아 있었으면 계좌가 의도와 다르게 중첩 노출

문서상 의문사항으로 남기기엔 위험도가 높다. PR1 범위에서 명시적으로 막아야 한다.

#### 수정 설계

1. `KisError::ManualIntervention(String)` 또는 이에 준하는 전용 오류 경로 추가
2. `trigger_manual_intervention(..., KeepLock)` 호출 경로는 상위에서 `abort_entry()`를 호출하지 않음
3. shared position lock 은 `Held(code)` 또는 별도 `ManualBlock(code)` 상태로 유지
4. `start_runner` 는 대상 종목 `status.manual_intervention_required == true` 이면 재시작 거부

예시:

```rust
match self.execute_entry(side, entry_price, stop_loss, take_profit).await {
    Ok(_) => return Ok(true),
    Err(KisError::ManualIntervention(msg)) => {
        error!("{}: manual_intervention 경로 — {}", self.stock_name, msg);
        return Err(KisError::ManualIntervention(msg));
    }
    Err(KisError::Preempted) => { /* 기존 경로 */ }
    Err(e) if e.is_retryable() => { /* 기존 경로 */ }
    Err(e) => {
        self.abort_entry(&sig, true, "fill_timeout_or_cutoff", current_price_at_signal).await;
        return Ok(false);
    }
}
```

핵심은 `manual_intervention`을 "일반 진입 실패"와 분리하는 것이다. 그렇지 않으면 status는 manual인데 lock은 풀린 모순 상태가 생긴다.

#### 검증

- 단위/리플레이 테스트:
  - `cancel_verify_failed` 발생 시 `position_lock`이 `Free`로 돌아가지 않는지
  - `status.manual_intervention_required=true` 상태에서 동일 종목 `/strategy/start`가 거부되는지
  - `manual_intervention` 후 다른 종목 진입이 막히는지

## 구현 순서 (의존성 고려)

우선순위: 위험도 순으로 배치하되, 공통 헬퍼를 먼저 둬야 나머지 수정이 깔끔해짐.

1. **Fix-3 + Fix-4 함께 먼저**: `trigger_manual_intervention`, `safe_orphan_cleanup`, 전용 manual 오류 경로 도입. live 포지션 보호 제거 회귀와 lock 해제 회귀를 동시에 차단.
2. **Fix-1 AES key capture**: `handle_control_message` 공통 헬퍼 추출. health gate가 실제로 동작해야 Fix-2의 Ready 전이가 의미를 가짐.
3. **Fix-2 Startup NotificationHealth 3-state**: `StrategyManager.system_degraded` → `notification_health` 전환 + start_runner Pending 거부 + startup failure 시 slipped runner stop. main.rs의 gate 스레드 호출부 수정.

각 단계마다 `cargo check` + 해당 경로 manual reasoning 검토.

## 수정 대상 파일 요약

| 파일 | 변경 |
|---|---|
| `src/infrastructure/websocket/connection.rs` | `handle_control_message` 공통 헬퍼 추출, 초기 loop + `handle_message` 양쪽에서 호출. `ControlMessage` 타입이 private 이면 pub(crate) 가시성 조정. |
| `src/strategy/live_runner.rs` | `trigger_manual_intervention`, `safe_orphan_cleanup`, `fetch_holding_snapshot` 추가. `check_and_restore_position`의 `Ok(None)` 분기, `check_and_restore_from_balance`, `execute_entry`의 cancel_verify_failed 분기를 공통 헬퍼로 치환. `manual_intervention` 전용 오류 경로 추가. `restore_position` 제거. |
| `src/presentation/routes/strategy.rs` | startup 전용 `NotificationHealth` enum 도입. `StrategyManager.system_degraded` → `notification_health` 필드 교체. `start_runner` 의 Pending/Failed 거부 로직 + `manual_intervention_required` 상태 재시작 거부. `mark_notification_ready` / `mark_notification_startup_failed` 추가. |
| `src/main.rs` | auto_start 스레드가 `mark_notification_ready` / `mark_notification_startup_failed` 를 호출하도록 수정. 폴링 간격 검토 (2초 × 15 = 30초로 확장 고려). |
| `src/domain/error.rs` | 필요 시 `KisError::ManualIntervention` variant 추가. |

## 테스트 및 검증 계획

### 자동 검증
1. `cargo check` — PR1 상태와 동일하게 에러 0.
2. `cargo clippy --lib --bin kis_agent` — 기존 대비 새 경고 없어야 함.
3. `cargo test --lib` — 86개 lib 테스트 green 유지.
4. (신규) `handle_control_message` 단위 테스트 추가 가능하면 추가. ControlMessage 생성이 어려우면 `parse_ws_message` 경로로 간접 테스트.
5. (신규) `manual_intervention` 경로 테스트:
   - `cancel_verify_failed` 시 `position_lock` 유지
   - `qty>0, avg=0` holdings를 보유로 판정
   - `NotificationHealth::Pending` 중 `/strategy/start` 거부
   - startup failure 시 Pending race로 먼저 떠버린 러너가 stop 되는지

### 수동 검증 (장 전 smoke test)

**Prereq**: PostgreSQL 기동, `.env`에 KIS appkey/secret + KIS_HTS_ID 설정, `KIS_AUTO_START` 미설정 (수동).

A. Health gate 정상 경로
- 서버 기동 → 로그 관찰
- 기대: `체결통보 AES key/iv 수신 완료` → `[health gate] 체결통보 준비 완료` → `[health gate] Notification Ready` → `[health gate] 통과 — KIS_AUTO_START 미설정 (수동 시작 대기)`

B. Health gate 실패 경로
- `KIS_HTS_ID` 를 일시적으로 비움 → 서버 기동
- 기대: 30초 뒤 `[health gate] Failed` + 모든 StrategyStatus degraded 플래그 + UI start 버튼 눌러도 "체결통보 불가" 에러

C. Pending race 차단
- 서버 기동 직후 0-5초 사이 UI 에서 수동 start 클릭
- 기대: "체결통보 health check 진행 중" 에러. 몇 초 뒤 다시 시도 → 정상 시작.

D. 재시작 복구 회귀 방지 (Fix-3)
- 모의투자 계정에 잔고 임의 포지션 + DB active_positions 수동 DELETE
- 서버 재시작 → 로그 관찰
- 기대: `잔고 N주 @ P원 감지 — 기존 미체결 주문 보존 + 수동 개입 전환` + UI degraded 표시. KIS HTS 에서 TP 주문이 **그대로 남아있어야 함**.

E. Manual intervention 락 보존 (Fix-4)
- `cancel_verify_failed` 또는 `safe_orphan_cleanup`의 holdings 존재 케이스를 강제로 유도
- 기대: 해당 종목 상태는 `manual_intervention_required=true`
- 기대: 반대 종목 시작 시도 시 shared lock 또는 start 정책으로 거부
- 기대: 같은 종목 재시작도 거부

## 백테스트/운영 판단

PR1의 완료 기준은 **기존 백테스트 수익률 유지**가 아니다. 현재 백테스트와 라이브는 여전히 아래 차이가 있다.

1. 백테스트 진입가 = `gap.mid_price`, 라이브 진입가 = `gap.top/bottom`
2. 백테스트 multi-stage = 하루 전체를 보고 가장 빠른 stage 하나를 선택, 라이브 multi-stage = 매 사이클 5m→15m→30m 순회
3. 주석상 "1차 손실 시 하루 종료"와 실제 구현이 다름

따라서 PR1 통과 의미는 다음으로 한정한다.

- cancel/fill race로 시스템 상태와 KIS 실제 상태가 갈라지지 않음
- startup gate가 실제로 작동함
- 재시작 시 live TP를 함부로 제거하지 않음
- 복구 불확실 시 manual_intervention으로 fail-safe

반대로 다음 결론은 아직 내리면 안 된다.

- "현재 `orb_fvg` 전략이 백테스트 기준으로 실거래에서도 잘 동작한다"
- "PR1 완료 후 완전 자동 운영이 가능하다"

2026-04-16 기준 운영 모드는 여전히 `KIS_AUTO_START=false` + 수동 시작이 맞다. 전략 edge와 백테스트-라이브 정합성은 `strategy-redesign-plan.md`의 PR2/PR3 범위다.

### 리플레이 (가능하면)
`docs/monitoring/2026-04-14.md`, `2026-04-15.md` 의 주요 이벤트 순서를 event_log 로 재생 → orphan position 0건 확인. PR1 기준으로는 통과했지만 본 수정 이후에도 회귀 없는지 재검증.

## 롤아웃 전략 (2026-04-16 KST 장 대응)

### 전제
- 오늘(2026-04-15) 남은 시간 동안 Fix-1/2/3/4 수정 + smoke test 완료 가능해야 배포.
- 실패 시 내일 장은 관찰 모드.

### 케이스 A — 전체 수정 완료 + smoke test 통과
1. PR1 수정분 포함 커밋 (사용자 승인 필수)
2. release 빌드 재생성
3. `KIS_AUTO_START=false` 유지 (계획서 Q2-B 합의: 종목은 둘 다 허용하되 수동)
4. 장 시작 전 서버 기동 → health gate 통과 확인
5. 장 시작 후 운영자 판단으로 122630/114800 수동 시작
6. 1거래 체결 후 의도대로 로그/잔고 검증

### 케이스 B — Fix-3 만 반영 + Fix-1/2 미완 (시간 부족)
1. Fix-3 만 반영하여 커밋 (live 포지션 보호 회귀만 차단)
2. 수동 시작 자체는 가능하지만 health gate가 실질적으로 동작 안 함
3. **추가 방어**: main.rs에서 `KIS_TRADING_ENABLED` 같은 별도 환경변수를 두고 기본 OFF. UI start도 이 변수가 ON일 때만 허용. 수동 관찰만 가능하게.

### 케이스 C — 시간 완전히 부족
1. PR1 자체를 roll back (soft reset, 변경사항 stash) 하거나 커밋하지 않은 상태로 두고 대신 운영 조치만
2. 기존 HEAD (25fe697) 상태로 2026-04-16 장 대응:
   - 서버는 기동하지 않음 또는 관찰 모드만
   - 운영자는 HTS/MTS에서 수동 매매만 수행
3. 주말/다음 날 동안 PR1 + 본 수정을 완성

판단 기준: 오늘 Fix-3 완료 + cargo test 통과 시점이 KST 23:00 이전이면 케이스 A, 아니면 B, 그보다 늦으면 C.

## 본 수정 이후 남는 후속 작업 (PR 재정비)

- **PR1 스코프**: 본 수정 반영분을 포함하여 "2026-04-15 Codex review 대응 포함 P0 핫픽스"로 통합 커밋.
- **PR2** (`orb_vwap_pullback`): 본 수정과 무관. 일정 지연. `strategy-redesign-plan.md` 그대로.
- **PR3** (ExecutionPolicy): 동일.
- **PR4** (리플레이/소액): 동일. 단, 리플레이 시나리오에 본 수정의 R1-R6 케이스를 추가.

본 수정이 끝나면 `docs/monitoring/strategy-redesign-plan.md` 의 PR1 완료 기준 중 "실행 안전성" 항목이 실제로 가까워진다:
- WS 체결통보 startup gate가 실제로 작동함: Fix-1+2
- 재시작 복구 시 balance-first 원칙으로 live TP를 함부로 제거하지 않음: Fix-3
- 복구 불확실/체결 불명 경로에서 manual_intervention이 일반 진입 실패로 흘러 락이 풀리지 않음: Fix-4

## 의문/확인 필요 사항

1. `ControlMessage` 와 `parser::WsMessage::Control` 의 가시성 — `handle_control_message` 시그니처 결정에 영향. private 이면 내부 타입 노출 없이 `&ctrl` 로 받거나 `fn handle_control(tr_id: &str, rt_cd: &str, output: Option<&serde_json::Value>)` 처럼 평탄화.
2. startup gate 실패와 runtime notification loss를 같은 상태 전이로 묶지 않기. runtime loss는 future work.
3. `manual_intervention` 전용 오류 경로를 `KisError` variant로 둘지, 로컬 enum으로 둘지 결정.
4. shared position lock을 `Held(code)` 재사용으로 막을지, `ManualBlock(code)` 같은 별도 상태를 둘지 결정.
5. 2026-04-16 운영 종목 Q2-B 합의 재확인 — 수동 시작 시 2종목 모두 허용 (lock 배타 유지). 본 수정은 이 합의와 독립적.

위 1-4는 구현 착수 시 첫 번째 파일 수정 전에 해당 코드 지점을 다시 읽고 확정.
