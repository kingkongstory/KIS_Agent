# 2026-04-24 장마감 데이터 분석 계획

## 목적

2026-04-24 장마감 이후 DB와 운영 이벤트를 기준으로 자동매매 로직이 정상 동작했는지, 신규 수정 사항이 의도대로 작동했는지, 남은 운영 리스크가 무엇인지 확인한다.

## 확인 범위

- `event_log` 장중/장외 이벤트 집계
- `trades`, `order_log`, `execution_journal` 거래·주문·체결 이력
- `active_positions` 잔존 포지션
- preflight 차단 사유와 quote sanity 이벤트
- WebSocket 재연결, API 오류, 장외 backoff 적용 여부
- OR/FVG/lock 관련 신호 발생 여부

## 산출물

- `docs/monitoring/2026-04-24-trading-postmortem.md`
- 완료 계획서 `docs/plans/done/2026-04-24-market-close-data-analysis.md`

## 완료 결과

- DB 기준 2026-04-24 장중 거래와 장외 이벤트를 분석했다.
- `114800` 1건 거래, 잔고 0, 수동개입 없음, parser/preflight 차단 재발 없음, post-flat reconcile 유예 동작을 확인했다.
- `entry_fill_timeout_ms=45,000` 부족, 청산 `balance_only` 지속, actor partial 복구 rule mismatch, 장외 balance polling 잔여 오류를 후속 리스크로 정리했다.
