# 실전 배포 체크리스트 (2026-04-16 KST 첫 투입)

본 문서는 `docs/monitoring/2026-04-16-production-readiness-plan.md` Phase 3 / Task 7 산출물이다.
운영자는 **이 문서만 보고** 장 시작 전 기동, 장중 모니터링, 장 종료 후 검증, 롤백 절차를 수행할 수 있어야 한다.

## 1. 기준

- 배포 시각: 2026-04-16 08:55 KST (장 시작 5분 전)
- 커밋 SHA: `<배포 직전 git rev-parse HEAD 값을 여기에 기입>`
- 빌드 파일: `target/release/kis_agent.exe`
- 빌드 명령: `cargo build --release --bin kis_agent`
- 허용 종목: `KIS_ALLOWED_CODES` 값 (권장 첫날 `122630` 단일)
- 최대 거래: `KIS_MAX_DAILY_TRADES_TOTAL=1`

## 2. `.env` 핵심 값 (실전 기준)

```
KIS_ENVIRONMENT=real
KIS_ENABLE_REAL_TRADING=true
KIS_APPKEY=<실전 appkey>
KIS_APPSECRET=<실전 appsecret>
KIS_ACCOUNT_NO=<실전 계좌번호>
KIS_HTS_ID=<HTS 로그인 ID — 체결통보 필수>

# 2026-04-16 가드
KIS_AUTO_START=false
KIS_ALLOWED_CODES=122630
KIS_MAX_DAILY_TRADES_TOTAL=1
KIS_REQUIRE_DB_IN_REAL=true
KIS_WS_STALE_SECS=120
KIS_WS_STALE_MESSAGE_SECS=240
KIS_RECONCILE_SECS=60
KIS_ENTRY_CUTOFF_REAL=15:00
KIS_REAL_OR_STAGES=15m

DATABASE_URL=postgres://postgres@localhost:5433/kis_agent
```

모의 burn-in 단계에서는 `KIS_ENVIRONMENT=paper`, `KIS_ENABLE_REAL_TRADING=false` 로 두고 나머지 가드는 동일.

## 3. Supervisor (Windows `nssm` 권장)

프로세스가 `exit(10)` (terminated WS) / `exit(11)` (tick stale) / `exit(3)` (실전 DB 필수) 코드로 종료되면 외부에서 자동 재기동해야 한다. 수동 운영이면 장중 주기적으로 프로세스 존재 여부를 확인.

```cmd
nssm install KIS_Agent C:\workspace_google\KIS_Agent\target\release\kis_agent.exe
nssm set KIS_Agent AppParameters server
nssm set KIS_Agent AppDirectory C:\workspace_google\KIS_Agent
nssm set KIS_Agent AppRestartDelay 5000
nssm set KIS_Agent AppExit Default Restart
```

## 4. 코드 검증 (배포 전 당일 마지막 회귀)

```bash
cd C:\workspace_google\KIS_Agent
cargo test --lib                                             # 145 pass 확인
cargo build --release --bin kis_agent                        # 빌드 성공
.\target\release\kis_agent.exe replay 122630 --date 2026-04-15 --rr 2.5
.\target\release\kis_agent.exe replay 114800 --date 2026-04-15 --rr 2.5
.\target\release\kis_agent.exe backtest 122630 --days 5 --parity passive --multi-stage
```

모든 명령이 `panic` 없이 종료되고, replay 세 경로(`legacy`, `parity-legacy`, `parity-passive`)가 출력되면 통과.

## 5. 장 시작 전 체크 (08:30~08:55)

| # | 항목 | 통과 기준 | 실패 시 조치 |
|---|---|---|---|
| 1 | PostgreSQL 기동 확인 | `psql ... -c "select 1"` | DB 기동 후 재시도. 실전 모드는 DB 없이 기동 불가 (`exit(3)`) |
| 2 | `.env` KIS_HTS_ID 값 실제 계정과 일치 | `echo $env:KIS_HTS_ID` | 값 교체. 없으면 체결통보 health gate 실패 |
| 3 | git 상태 clean | `git status` 에 미커밋 변경 없음 | 배포 직전 SHA 를 단일 커밋으로 고정 |
| 4 | 서버 기동 | `.\target\release\kis_agent.exe server` | stdout 로그에 다음이 순서대로 떠야 함:<br>- `PostgreSQL 연결 완료`<br>- `자동매매 허용 종목: ["122630"]` (또는 설정값)<br>- `KIS WebSocket 실시간 스트리밍 시작`<br>- `잔고 reconciliation 스케줄 기동 — 간격 60초`<br>- `[health gate] 체결통보 준비 완료` (30초 이내)<br>- `[health gate] Notification Ready`<br>- `[health gate] 통과 — auto_start=false (수동 시작 대기)` |
| 5 | `/api/v1/monitoring/health` 확인 | `db_connected: true`, `ws.notification_ready: true`, `ws_tick_stale: false` | degraded 경우 원인 조치 (DB, HTS_ID, WS 재연결) |
| 6 | UI `/` 또는 `/api/v1/strategy/status` | 종목별 degraded=false, manual_intervention_required=false | true 면 8번 수동 시작 차단 |

## 6. 장 시작 절차 (09:00)

1. 09:00:00 장 개시 확인 (`/api/v1/monitoring/health` 의 ws.last_tick_epoch_secs 갱신).
2. 09:06 Yahoo OR 5m 교체 시도 로그 확인 (`Yahoo OR 교체 성공` 또는 실패 fallback).
3. 09:16 15m OR 확정. `/api/v1/strategy/status` 에 `or_stages` 가 5m/15m 두 건 이상.
4. 09:20 이후 FVG 탐색 시작. 실전 모드면 `15m` stage 만 필터링.
5. 운영자가 **수동**으로 `/api/v1/strategy/start` POST (또는 UI Start 버튼).
   - 허용 종목이 아니면 거부됨 (`허용 종목 목록(KIS_ALLOWED_CODES) 밖`).
   - 수동 개입 상태면 거부됨 (`수동 개입 필요`).
   - `KIS_ENABLE_REAL_TRADING=false` 이면 거부됨 (`실전 주문 opt-in 미활성`).

## 7. 장중 중단 조건 (즉시 stop 또는 프로세스 교체)

| 조건 | 감지 경로 | 조치 |
|---|---|---|
| WS 영구 종료 (`terminated=true`) | watchdog 로 `exit(10)` 자동 | supervisor 가 자동 재기동 |
| 장중 tick stale (`tick_age > 120s`) | watchdog 로 `exit(11)` 자동 | supervisor 가 자동 재기동 |
| 잔고 불일치 | 60초 내 `balance_reconcile_mismatch` critical 이벤트 → 해당 러너만 `manual_intervention` 전환 | HTS 에서 실제 포지션/주문 확인 후 수동 청산, 프로세스 재기동 금지하고 DB 확인 |
| cancel/fill race 감지 | `cancel_fill_race_detected` 이벤트 | HTS 잔고 확인, 해당 종목 수동 SL 설정 후 서버 수동 stop |
| 일일 최대 손실 한도 (-1.5%) | 러너 자체 중단 | 해당 러너는 자동 stop, 기록 확인 |
| 1회 진입 완료 | `max_daily_trades_override=1` 에 의해 러너 자동 중단 | 다른 종목 시작 가능 (`KIS_ALLOWED_CODES` 에 있으면) |

## 8. 장 종료 후 검증 (15:35~)

1. **프로세스 종료 확인**: 15:35 이후에도 정상 실행 중이면 수동으로 `Ctrl+C` 또는 `taskkill` (`/F` 금지 — orphan 주문 유발). graceful shutdown 30초 대기.
2. **일일 결산 리포트**: `/api/v1/monitoring/daily-report?from=<today>` 로 `total_trades`, `total_pnl_pct`, `ws_reconnect_count`, `api_error_count` 확인.
3. **이벤트 조회**: `/api/v1/monitoring/events?date=<today>` 에서 severity=critical 이 없어야 함.
4. **KIS HTS 잔고 대조**: 시스템 DB `active_positions` 와 HTS 잔고가 동일해야 함. 차이 있으면 manual_intervention 이벤트 여부 확인.
5. **Yahoo 5분봉 누적 수집**:
   ```
   .\target\release\kis_agent.exe collect-yahoo --codes 122630,114800 --interval 5m --range 1d
   ```

## 9. 롤백 절차

배포 당일 장중 이상 → 즉시 `KIS_AUTO_START=false` 유지한 채 stop 만 수행 (수동 시작이므로 무동작). 러너가 돌고 있다면 `/api/v1/strategy/stop` POST.

장 종료 후 코드 롤백이 필요한 경우:

```bash
git log --oneline               # 배포 직전 SHA 확인
git reset --hard <이전 SHA>     # 주의: 미커밋 변경 있으면 stash 먼저
cargo build --release --bin kis_agent
```

## 10. Go / No-Go 최종 판단 플로우

- Phase 1~3 모두 완료?
- `cargo test --lib` 전부 통과?
- 모의투자 3영업일 연속 무사고? (`critical` 이벤트 0건, `balance_reconcile_mismatch` 0건)
- 본 문서 §4 코드 검증 통과?
- 배포 SHA 고정 커밋 완료?

위 5개가 전부 Yes 면 **Go**. 하나라도 아니면 당일은 **수동 관찰 모드**로 운영하고 PR2/PR3 일정과 함께 재조정.

## 11. 연락/보고 체크

- 장중 `critical` 이벤트 발생 시 즉시 운영자 알림 (Slack/메일)
- 장 종료 후 당일 리포트 정리 → `docs/monitoring/<YYYY-MM-DD>.md` 에 요약 기록
- `docs/monitoring/2026-04-16-production-readiness-plan.md` 의 수용 기준 대비 실제 동작 내역을 문서화

---

**원칙**: 본 체크리스트에 없는 임기응변은 하지 않는다. 의심스러우면 즉시 수동 stop + HTS 수동 확인. 자동 복구보다 **정지 + 사람 개입** 이 우선.
