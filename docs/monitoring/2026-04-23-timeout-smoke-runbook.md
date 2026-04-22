# 2026-04-23 Timeout Smoke 운영 Runbook

## 목적

2026-04-23 장중 paper smoke 에서 timeout evidence 를 수집하기 전에
장전 준비 항목을 고정된 명령으로 점검하기 위한 runbook 이다.

관련 스크립트:

- [check_timeout_smoke_readiness.ps1](/C:/workspace_google/KIS_Agent/scripts/check_timeout_smoke_readiness.ps1)
- [start_timeout_smoke_server.ps1](/C:/workspace_google/KIS_Agent/scripts/start_timeout_smoke_server.ps1)
- [collect_timeout_smoke.ps1](/C:/workspace_google/KIS_Agent/scripts/collect_timeout_smoke.ps1)
- [run_timeout_smoke_verdicts.ps1](/C:/workspace_google/KIS_Agent/scripts/run_timeout_smoke_verdicts.ps1)

## 장전 체크리스트

### 1. PostgreSQL

Windows 서비스가 아니라 수동 기동 기준이다.

```powershell
C:\tools\pgsql\bin\pg_ctl.exe status -D C:\tools\pgsql\data
```

중지 상태면:

```powershell
C:\tools\pgsql\bin\pg_ctl.exe start -D C:\tools\pgsql\data -l C:\tools\pgsql\log.txt
```

TimeZone 확인:

```powershell
psql postgres://postgres@localhost:5433/kis_agent -X -A -t -c "SELECT current_setting('TimeZone');"
```

기대값은 `Asia/Seoul`.

### 2. 바이너리 / 로그 경로

서버 바이너리 표준 경로:

```text
target/release/kis_agent.exe
```

Timeout smoke 표준 로그 경로:

```text
logs/smoke-YYYY-MM-DD-server.log
```

### 3. readiness 스냅샷

아래 스크립트는 PostgreSQL, DB timezone, 바이너리, 서버 health, WS readiness 를 한 번에 점검한다.

```powershell
scripts/check_timeout_smoke_readiness.ps1 `
  -OutFile logs/timeout-smoke-readiness-2026-04-23.json
```

판정 기준:

- `Ready (pre-server) = True`
  서버 시작 전 최소 조건 충족
- `Ready (before-phase) = True`
  `collect_timeout_smoke.ps1 -Phase before` 를 실행해도 되는 상태

## 서버 기동

표준 서버 기동은 아래 스크립트로 고정한다.

```powershell
scripts/start_timeout_smoke_server.ps1 `
  -EnsurePostgres `
  -WaitTimeoutSec 45
```

이 스크립트는 아래를 수행한다.

- 필요 시 PostgreSQL 기동
- `target/release/kis_agent.exe server` 백그라운드 실행
- 로그를 `logs/smoke-YYYY-MM-DD-server.log` 로 기록
- PID 메타를 `logs/smoke-YYYY-MM-DD-server.pid.json` 에 저장
- `/api/v1/monitoring/health` 기준
  - `db_connected=true`
  - `ws.notification_ready=true`
  - `ws_tick_stale=false`
  - `ws.terminated=false`
  를 기다린 뒤 반환

실제 기동 없이 명령만 확인:

```powershell
scripts/start_timeout_smoke_server.ps1 -DryRun
```

## 권장 운영 순서

### 08:50

1. `check_timeout_smoke_readiness.ps1`
2. 필요 시 `start_timeout_smoke_server.ps1`
3. 다시 `check_timeout_smoke_readiness.ps1`

### 09:00 직후

자연 발생 확률이 상대적으로 높은 `armed-timeout` 부터 시작한다.

```powershell
scripts/collect_timeout_smoke.ps1 `
  -Phase before `
  -ScenarioId armed-timeout `
  -StockCode 122630 `
  -ServerLogPath logs/smoke-2026-04-23-server.log
```

### 시나리오 종료 직후

```powershell
scripts/collect_timeout_smoke.ps1 `
  -Phase after `
  -ScenarioId armed-timeout `
  -StockCode 122630 `
  -ServerLogPath logs/smoke-2026-04-23-server.log
```

```powershell
scripts/run_timeout_smoke_verdicts.ps1 `
  -RunDir docs/monitoring/evidence/2026-04-23-timeout-smoke/run-01
```

## 시나리오 우선순위

1. `armed-timeout`
2. `entry-fill-timeout`
3. `exit-fill-timeout`

`exit-fill-timeout` 은 자연 발생 확률이 가장 낮다. 3세션 내 증거를 못 잡으면
네트워크 지연이나 paper 응답 지연 유도 방안을 별도로 설계한다.

## 2026-04-22 현재 점검 상태

2026-04-22 기준 수동 점검 결과:

- PostgreSQL: 실행 중
- `target/release/kis_agent.exe`: 존재
- `kis_agent.exe server --help`: 정상
- `http://127.0.0.1:3000/api/v1/monitoring/health`: 서버 미기동 상태라 연결 거부

즉, 남은 미결정 사항은 코드가 아니라 장중 운영 절차였다. 이 문서와 스크립트로
그 절차를 고정한다.
