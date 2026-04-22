-- 계획서 §5.2 판정 — ExitFill timeout → manual 정합성.
-- 파라미터: :stock_code, :t_from, :t_to, :ap_manual, :ap_state, :ap_last_exit_order, :ap_last_exit_reason
--
-- 합격 조건 (AND):
--   (1) actor_timeout_expired.metadata.phase='ExitFill' 이 창 내 1건 이상
--   (2) manual_intervention_required 이벤트가 (1) 이후에 존재
--   (3) active_positions.manual_intervention_required = true
--   (4) active_positions.execution_state = 'manual_intervention'
-- NA: (1) 이 0건 (시나리오 C 아님)
-- FAIL: (1) 있는데 (2/3/4) 중 하나라도 불일치

\set ON_ERROR_STOP on

-- 1. exit timeout 이벤트
WITH exit_timeout AS (
    SELECT event_time, metadata
    FROM event_log
    WHERE event_time BETWEEN :t_from ::timestamptz AND :t_to ::timestamptz
      AND stock_code = :stock_code
      AND event_type = 'actor_timeout_expired'
      AND metadata->>'phase' = 'ExitFill'
),
-- 2. manual 이벤트 (timeout 이후)
manual_ev AS (
    SELECT event_time, metadata
    FROM event_log
    WHERE event_time BETWEEN :t_from ::timestamptz AND :t_to ::timestamptz
      AND stock_code = :stock_code
      AND event_type = 'manual_intervention_required'
),
counts AS (
    SELECT
        (SELECT COUNT(*) FROM exit_timeout) AS timeout_count,
        (SELECT COUNT(*) FROM manual_ev)    AS manual_event_count,
        (SELECT MIN(event_time) FROM exit_timeout) AS first_timeout_at,
        (SELECT MAX(event_time) FROM manual_ev)    AS last_manual_at,
        CAST(:'ap_manual' AS boolean) AS archived_ap_manual,
        :'ap_state'                   AS archived_ap_state,
        :'ap_last_exit_order'         AS archived_ap_last_exit_order,
        :'ap_last_exit_reason'        AS archived_ap_last_exit_reason
)
SELECT * FROM counts;

-- Verdict
WITH exit_timeout AS (
    SELECT event_time
    FROM event_log
    WHERE event_time BETWEEN :t_from ::timestamptz AND :t_to ::timestamptz
      AND stock_code = :stock_code
      AND event_type = 'actor_timeout_expired'
      AND metadata->>'phase' = 'ExitFill'
),
manual_ev AS (
    SELECT event_time
    FROM event_log
    WHERE event_time BETWEEN :t_from ::timestamptz AND :t_to ::timestamptz
      AND stock_code = :stock_code
      AND event_type = 'manual_intervention_required'
),
v AS (
    SELECT
        (SELECT COUNT(*) FROM exit_timeout) AS to_c,
        (SELECT COUNT(*) FROM manual_ev WHERE event_time >= (SELECT MIN(event_time) FROM exit_timeout)) AS after_manual_c,
        CAST(:'ap_manual' AS boolean) AS ap_m,
        :'ap_state'                   AS ap_s
)
SELECT
    to_c,
    after_manual_c,
    ap_m,
    ap_s,
    CASE
        WHEN to_c = 0 THEN 'NA'
        WHEN to_c >= 1 AND after_manual_c >= 1 AND ap_m = true AND ap_s = 'manual_intervention' THEN 'PASS'
        ELSE 'FAIL'
    END AS verdict
FROM v;
