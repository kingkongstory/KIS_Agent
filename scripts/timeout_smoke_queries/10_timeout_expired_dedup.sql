-- 계획서 §5.1 판정 — actor_timeout_expired 가 동일 시간창/종목에서 정확히 1건인지 검증.
-- 파라미터: :stock_code, :t_from, :t_to (문자열, psql -v 로 전달)
--
-- 합격:
--   - actor_timeout_expired 가 1건
--   - metadata.phase 가 기대값 (EntryFill | ExitFill) — 시나리오 맥락에서 운영자가 해석
--   - 이후 actor_invalid_transition / actor_rule_mismatch 가 같은 stock_code 에 없음
-- 실패:
--   - actor_timeout_expired >= 2
--   - actor_invalid_transition / actor_rule_mismatch 동반
-- NA:
--   - actor_timeout_expired == 0 (시나리오 A 처럼 timeout 관련 증거가 없는 경우)

\set ON_ERROR_STOP on

-- 1. 증거 row 목록 (raw timeline)
SELECT
    event_time,
    stock_code,
    event_type,
    severity,
    message,
    metadata,
    metadata->>'phase' AS phase
FROM event_log
WHERE event_time BETWEEN :t_from ::timestamptz AND :t_to ::timestamptz
  AND stock_code = :stock_code
  AND event_type IN (
      'actor_timeout_expired',
      'actor_invalid_transition',
      'actor_rule_mismatch'
  )
ORDER BY event_time;

-- 2. Verdict
WITH timeout_hits AS (
    SELECT event_time
    FROM event_log
    WHERE event_time BETWEEN :t_from ::timestamptz AND :t_to ::timestamptz
      AND stock_code = :stock_code
      AND event_type = 'actor_timeout_expired'
),
first_timeout AS (
    SELECT MIN(event_time) AS at
    FROM timeout_hits
),
post_timeout_warnings AS (
    SELECT event_type
    FROM event_log
    WHERE event_time BETWEEN :t_from ::timestamptz AND :t_to ::timestamptz
      AND stock_code = :stock_code
      AND event_type IN ('actor_invalid_transition', 'actor_rule_mismatch')
      AND event_time >= (SELECT at FROM first_timeout)
)
SELECT
    (SELECT COUNT(*) FROM timeout_hits) AS timeout_count,
    COUNT(*) FILTER (WHERE event_type = 'actor_invalid_transition') AS invalid_count_after_timeout,
    COUNT(*) FILTER (WHERE event_type = 'actor_rule_mismatch')      AS mismatch_count_after_timeout,
    CASE
        WHEN (SELECT COUNT(*) FROM timeout_hits) = 0 THEN 'NA'
        WHEN (SELECT COUNT(*) FROM timeout_hits) = 1
         AND COUNT(*) FILTER (WHERE event_type = 'actor_invalid_transition') = 0
         AND COUNT(*) FILTER (WHERE event_type = 'actor_rule_mismatch') = 0 THEN 'PASS'
        ELSE 'FAIL'
    END AS verdict
FROM post_timeout_warnings;
