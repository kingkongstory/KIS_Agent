# 2026-04-24 서버 준비 상태 점검

## 목적

- 현재 서버가 실제로 기동 중인지 확인한다.
- HTTP 응답, 전략 상태, 최근 로그를 함께 점검해 단순 기동이 아니라 운용 준비 상태인지 판정한다.
- 발견된 문제와 즉시 조치가 필요한 항목을 정리한다.

## 점검 범위

1. 프로세스/포트/HTTP health 응답
2. 전략 상태 및 실시간 구독 준비 상태
3. 최근 로그와 에러 징후
4. 최종 판정과 즉시 조치 항목

## 점검 결과

### 1. 프로세스/포트/HTTP

- `kis_agent.exe server` 프로세스가 실행 중이며 `127.0.0.1:3000` listen 확인
- `GET /api/v1/strategy/status` 정상 응답
- `GET /api/v1/monitoring/health` 정상 응답
- `GET /health` 는 404
  - 현재 실제 health endpoint 는 `/api/v1/monitoring/health`

### 2. 현재 운용 상태

- 현재 시각: `2026-04-24 01:02 KST`
- 환경: `KIS_ENVIRONMENT=paper`
- `KIS_ENABLE_REAL_TRADING=false`
- `KIS_AUTO_START=true`
- `notification_ready=true`
- `db_connected=true`
- `active_runners=[]`
- 전략 상태 API 기준 `122630`, `114800` 모두 `active=false`, `state="종료"`
- `active_positions` 테이블 잔존 행 없음

### 3. 중요한 판정

- 서버는 살아 있고 KIS notification startup gate 도 통과한 상태다.
- 그러나 현재 러너가 하나도 없으므로 “지금 바로 자동매매가 돌 준비가 된 상태”는 아니다.
- 더 중요한 점은 `auto_start=true` 가 장 시작 스케줄이 아니라 **서버 시작 시 1회 실행**이라는 점이다.
- 현재 서버 프로세스는 `2026-04-23 22:31:04 KST` 에 시작됐다.
- 해당 시점에 auto start 가 한 번 실행됐고, 이벤트 로그 기준:
  - `114800`: `2026-04-23 22:31:14 KST` 전후 `초기화 -> 장 시작 대기 -> OR 수집 중 (5분) -> 신호 탐색 -> 종료`
  - `122630`: `2026-04-23 22:31:17 KST` 전후 동일하게 `종료`
- 따라서 이 상태로 밤을 넘기면 **2026-04-24 장 시작에 자동으로 다시 살아나지 않는다.**

### 4. 야간 이상 징후

- `/api/v1/monitoring/health` 기준 오늘 누적:
  - `ws_reconnect=40`
  - `api_error=27`
- 최근 3시간 DB 집계:
  - `ws_reconnect=105`
  - `api_error=27`
  - `phase_change=8`
- 최근 이벤트에는 장외 시간에도 `oauth2/Approval`, `inquire-price`, `inquire-balance` HTTP 실패와 WS reconnect 가 계속 기록됨
- `last_message_epoch_secs` 는 `2026-04-24 01:02:08 KST` 로 갱신되고 있으나,
  - `last_tick_epoch_secs=null`
  - `last_notification_epoch_secs=null`
  - 장외라 치명적 실패로 보진 않지만, “장중 데이터 수신이 검증된 상태”는 아니다.

## 최종 판정

- `서버 기동`: 정상
- `DB 연결`: 정상
- `startup notification gate`: 정상
- `잔존 포지션/manual 잔재`: 없음
- `오늘 장 자동매매 준비`: 미완료

이유는 러너가 모두 종료 상태이며, 현재 구조상 장 시작에 auto start 가 다시 실행되지 않기 때문이다.

## 즉시 조치

1. 오늘 장전에 수동으로 `POST /api/v1/strategy/start` 를 두 종목에 다시 호출하거나 서버를 장전 재시작해야 한다.
2. 근본적으로는 장 시작 시각 기반 start scheduler 또는 장전 heartbeat/cron 이 필요하다.
3. 장외 WS reconnect / REST polling 노이즈는 별도 수정이 필요하다.
