# Timeout Smoke Evidence

[2026-04-21 timeout smoke 수집 계획서](../2026-04-21-timeout-smoke-data-collection-plan.md) 의 운영용 작업 폴더이다. 장중 paper smoke 세션마다 이 폴더 밑에 증거 스냅샷을 남긴다.

## 디렉터리 규칙

```
docs/monitoring/evidence/
  templates/
    manifest.json      # 세션 메타 스켈레톤
    notes.md           # 운영자 메모 템플릿
  YYYY-MM-DD-timeout-smoke/
    run-01/
      manifest.json
      notes.md
      health.json
      strategy-status-before.json
      strategy-status-after.json
      armed-stats-before.json
      armed-stats-after.json
      active-positions-before.json
      active-positions-after.json
      event-log.csv
      execution-journal.csv
      server.log
    run-02/ ...
```

- 한 세션에서 시나리오를 여러 번 돌리면 `run-02`, `run-03` 으로 분리
- **종목별로 run 분리 권장**: `-StockCode 122630` / `-StockCode 114800` 을 각각 `-Phase before` 로 호출하면 `run-01` / `run-02` 로 자동 분리된다. 두 종목의 증거를 한 run 에 섞지 않는다 (evidence 파일들이 `-StockCode` 기준 1건만 뽑기 때문에 섞으면 한 종목 증거가 덮어써진다).

## 생성 방법

증거 수집은 [`scripts/collect_timeout_smoke.ps1`](../../../scripts/collect_timeout_smoke.ps1) 로 자동화한다.

```powershell
# 세션 시작 직후 (시나리오 진입 전)
scripts/collect_timeout_smoke.ps1 `
  -Phase before `
  -ScenarioId armed-timeout `
  -StockCode 122630 `
  -ServerBaseUrl http://127.0.0.1:3000 `
  -DbUrl "postgres://postgres@localhost:5433/kis_agent"

# 시나리오 종료 직후
scripts/collect_timeout_smoke.ps1 `
  -Phase after `
  -ScenarioId armed-timeout `
  -StockCode 122630 `
  -ServerBaseUrl http://127.0.0.1:3000 `
  -DbUrl "postgres://postgres@localhost:5433/kis_agent" `
  -ServerLogPath logs/server.log
```

`-Phase before` 에서 `manifest.json` 의 `run_started_at_kst` 가 기록되고, `-Phase after` 는 그 값을 기준으로 `event-log.csv` / `execution-journal.csv` 의 시간창을 잡는다.

### 운영 팁

- **`-ScenarioId` 는 하나만 태깅해도 증거는 전부 담긴다.** before 단계에서 `armed-timeout` 으로 태깅했는데 장중에 `exit-fill-timeout` 이 먼저 터져도 `event-log.csv` / `execution-journal.csv` 에는 모든 시나리오 이벤트가 시간창 기준으로 수집된다. `run_timeout_smoke_verdicts.ps1` 은 세 시나리오 verdict 를 독립 계산하므로, 발생한 시나리오는 PASS/FAIL, 미발생 시나리오는 NA 로 나온다. 즉 ScenarioId 는 manifest 메타 용도이고 운영상 오분류는 무해.
- **`-Phase after` 는 시나리오 직후 호출을 원칙으로 한다.** 장마감 후 일괄 호출(15:35 경)은 증거 보존 용도로는 가능하지만, `before` 부터 `after` 까지의 창이 길어지면서 같은 종목의 다른 timeout/event 가 섞일 수 있다. 현재 verdict 는 그 전체 창을 그대로 사용하므로 `timeout_count` 나 warning count 가 달라져 false FAIL 이 날 수 있다. 따라서 공식 PASS/FAIL 판정용 evidence 는 시나리오 직후 `-Phase after` 로 닫는 편이 안전하다.
- **서버 graceful 종료는 [`scripts/stop_timeout_smoke_server.ps1`](../../../scripts/stop_timeout_smoke_server.ps1) 사용.** PID 파일에서 wrapper 자식 `kis_agent.exe` 를 찾아 `taskkill /PID`(없이 `/F`) 로 CLOSE_EVENT 를 보낸다. Rust `tokio::signal::ctrl_c()` 핸들러를 트리거해 체결 대기 루프 탈출 → cancel → `wait_all_stopped(40s)` 경로를 탄다. CLAUDE.md 의 "`taskkill /F` 지양" 원칙을 그대로 따른다.

## 판정

합격/실패 판정은 [`scripts/run_timeout_smoke_verdicts.ps1`](../../../scripts/run_timeout_smoke_verdicts.ps1)
로 수행한다. 이 스크립트는 `manifest.json`, archived
`active-positions-after.json`, `event-log.csv`, `execution-journal.csv`
만으로 verdict 를 재계산한다. 즉 공식 판정 경로는 live DB 재조회가 아니라
archived evidence only 이다.

```powershell
scripts/run_timeout_smoke_verdicts.ps1 `
  -RunDir docs/monitoring/evidence/2026-04-22-timeout-smoke/run-01
```

실행 후 run 폴더에 아래 파일이 추가된다.

- `verdict-10-timeout-expired.txt`
- `verdict-20-exit-manual.txt`
- `verdict-30-armed-detail.txt`

각 결과의 마지막 줄에 `verdict` 열로 `PASS` / `FAIL` / `NA` 가 들어간다.

필수 archived 파일이 export 실패 payload (`{ "error": ... }` 또는 `error,...`) 를
포함하면 wrapper 는 verdict 를 내지 않고 `evidence_incomplete:` 오류로 즉시 중단한다.
이 경우 전략 FAIL 로 해석하면 안 되고, 먼저 증거 수집 실패를 복구해야 한다.

## 시나리오 매핑

| ScenarioId | 계획서 2.x | 확인 판정 |
|---|---|---|
| `armed-timeout` | 2.1 A | 5.3 `armed_entry_submit_timeout` |
| `entry-fill-timeout` | 2.2 B | 5.1 `actor_timeout_expired` 단일 |
| `exit-fill-timeout` | 2.3 C | 5.1 + 5.2 manual 정합성 |

## 제출물 체크리스트

세션 종료 후 아래가 모두 있는지 확인한 뒤 커밋한다.

- [ ] `manifest.json` (시작/종료 시각, 시나리오, 환경, 종목)
- [ ] `notes.md` (장중 특이사항, 수동 조작 여부)
- [ ] `health.json`
- [ ] `strategy-status-before.json`, `strategy-status-after.json`
- [ ] `armed-stats-before.json`, `armed-stats-after.json`
- [ ] `active-positions-before.json`, `active-positions-after.json`
- [ ] `event-log.csv`, `execution-journal.csv`
- [ ] `server.log`
- [ ] 판정 결과 (`YYYY-MM-DD-timeout-smoke-result.md`)
