# Timeout Smoke 판정 쿼리

[계획서 §5 합격/실패 기준](../../docs/monitoring/2026-04-21-timeout-smoke-data-collection-plan.md) 을
SQL 로 자동 판정한다.

## 파라미터

세 쿼리 모두 동일한 3개 `psql -v` 변수를 받는다. 변수는 문자열이라 작은따옴표까지 감싸서 전달해야 한다.

| 변수 | 예시 | 의미 |
|---|---|---|
| `stock_code` | `"'122630'"` | 대상 종목코드 |
| `t_from` | `"'2026-04-22T09:00:00'"` | 시간창 시작 (KST, `timestamptz` 캐스팅됨) |
| `t_to`   | `"'2026-04-22T09:30:00'"` | 시간창 끝 |

## 권장 실행

재현 가능한 공식 verdict 는 SQL 직접 실행이 아니라 archived evidence wrapper 로 얻는다.
이 wrapper 는 `manifest.json`, `active-positions-after.json`, `event-log.csv`,
`execution-journal.csv` 만으로 verdict 를 계산하며 live DB 를 다시 읽지 않는다.

```powershell
scripts/run_timeout_smoke_verdicts.ps1 `
  -RunDir docs/monitoring/evidence/2026-04-22-timeout-smoke/run-01
```

archived 파일 중 하나라도 export 실패 payload (`{ "error": ... }` 또는 `error,...`) 이면
wrapper 는 `evidence_incomplete:` 오류로 중단한다.

## 수동 사용 예

아래 SQL 직접 실행 예시는 live DB 를 수동 탐색하거나 wrapper 구현을 대조할 때만 사용한다.
공식 archived verdict 경로로는 권장하지 않는다.

```powershell
$m = Get-Content run-01/manifest.json -Raw | ConvertFrom-Json
psql $env:DATABASE_URL `
  -v stock_code="'$($m.stock_code)'" `
  -v t_from="'$($m.run_started_at_kst)'" `
  -v t_to="'$($m.run_ended_at_kst)'" `
  -f scripts/timeout_smoke_queries/10_timeout_expired_dedup.sql
```

`20_exit_manual_consistency.sql` 은 live DB 대신 archived snapshot 값을 사용한다.
수동 실행 시 아래 변수를 추가로 넘겨야 한다.

| 변수 | 예시 | 의미 |
|---|---|---|
| `ap_manual` | `"true"` | archived `active_positions-after.manual_intervention_required` |
| `ap_state` | `"manual_intervention"` | archived `active_positions-after.execution_state` |
| `ap_last_exit_order` | `"1234567890"` | archived `active_positions-after.last_exit_order_no` |
| `ap_last_exit_reason` | `"stop_loss"` | archived `active_positions-after.last_exit_reason` |

## 쿼리 → 판정 매핑

| 파일 | 계획서 § | 대상 시나리오 |
|---|---|---|
| [`10_timeout_expired_dedup.sql`](10_timeout_expired_dedup.sql) | 5.1 | B (entry-fill-timeout), C (exit-fill-timeout) |
| [`20_exit_manual_consistency.sql`](20_exit_manual_consistency.sql) | 5.2 | C (exit-fill-timeout) |
| [`30_armed_detail_check.sql`](30_armed_detail_check.sql) | 5.3 | A (armed-timeout) |

각 쿼리의 마지막 줄은 `verdict` 열로 `PASS` / `FAIL` / `NA` 를 반환한다. `NA` 는 "해당 시나리오 증거 자체가 창 안에 없음" 을 의미하며, 시나리오와 무관한 쿼리를 다른 시나리오에 돌렸을 때 나온다.
