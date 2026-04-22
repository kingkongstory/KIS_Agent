-- 계획서 §5.3 판정 — armed_entry_submit_timeout detail 기록 확인.
-- 파라미터: :stock_code, :t_from, :t_to
--
-- 합격 조건 (AND):
--   (1) execution_journal.phase='armed_wait_exit' 이 창 내 1건 이상
--   (2) 그 중 metadata.detail = 'armed_entry_submit_timeout' 이 1건 이상
--   (3) 같은 signal_id 의 armed_wait_enter 가 (1) 보다 앞서 존재
--   (4) 대응하는 event_log.armed_watch_exit.metadata.detail 도 동일값 (불일치 방지)
-- NA: (1)/(2) 둘 다 0 (시나리오 A 아님)
-- FAIL: (1) 있는데 (2/3/4) 중 하나라도 실패

\set ON_ERROR_STOP on

-- 1. journal armed_wait_exit
WITH awx AS (
    SELECT id, created_at, signal_id, resolution_source, reason, metadata,
           metadata->>'detail'  AS detail,
           metadata->>'outcome' AS outcome
    FROM execution_journal
    WHERE created_at BETWEEN :t_from ::timestamptz AND :t_to ::timestamptz
      AND stock_code = :stock_code
      AND phase = 'armed_wait_exit'
),
-- 2. journal armed_wait_enter 선행 여부
awe AS (
    SELECT signal_id, MIN(created_at) AS enter_at
    FROM execution_journal
    WHERE created_at BETWEEN :t_from ::timestamptz AND :t_to ::timestamptz
      AND stock_code = :stock_code
      AND phase = 'armed_wait_enter'
    GROUP BY signal_id
),
-- 4. event_log armed_watch_exit 의 detail 불일치 여부
ele AS (
    SELECT metadata->>'signal_id' AS signal_id,
           metadata->>'detail'    AS detail,
           event_time
    FROM event_log
    WHERE event_time BETWEEN :t_from ::timestamptz AND :t_to ::timestamptz
      AND stock_code = :stock_code
      AND event_type = 'armed_watch_exit'
),
summary AS (
    SELECT
        awx.signal_id,
        awx.created_at AS journal_exit_at,
        awx.outcome    AS journal_outcome,
        awx.detail     AS journal_detail,
        awe.enter_at   AS journal_enter_at,
        EXISTS (
            SELECT 1
            FROM ele l
            WHERE l.signal_id = awx.signal_id
        ) AS event_exit_present,
        EXISTS (
            SELECT 1
            FROM ele l
            WHERE l.signal_id = awx.signal_id
              AND COALESCE(l.detail, '') = COALESCE(awx.detail, '')
        ) AS event_detail_match,
        ele.detail     AS event_detail,
        ele.event_time AS event_exit_at
    FROM awx
    LEFT JOIN awe ON awe.signal_id = awx.signal_id
    LEFT JOIN ele ON ele.signal_id = awx.signal_id
    ORDER BY awx.created_at
)
SELECT * FROM summary;

-- Verdict
WITH awx AS (
    SELECT id, signal_id, created_at, metadata->>'detail' AS detail, metadata->>'outcome' AS outcome
    FROM execution_journal
    WHERE created_at BETWEEN :t_from ::timestamptz AND :t_to ::timestamptz
      AND stock_code = :stock_code
      AND phase = 'armed_wait_exit'
),
awe AS (
    SELECT signal_id, MIN(created_at) AS enter_at
    FROM execution_journal
    WHERE created_at BETWEEN :t_from ::timestamptz AND :t_to ::timestamptz
      AND stock_code = :stock_code
      AND phase = 'armed_wait_enter'
    GROUP BY signal_id
),
ele AS (
    SELECT metadata->>'signal_id' AS signal_id, metadata->>'detail' AS detail
    FROM event_log
    WHERE event_time BETWEEN :t_from ::timestamptz AND :t_to ::timestamptz
      AND stock_code = :stock_code
      AND event_type = 'armed_watch_exit'
),
v AS (
    SELECT
        (SELECT COUNT(*) FROM awx) AS exit_c,
        (SELECT COUNT(*) FROM awx WHERE detail = 'armed_entry_submit_timeout') AS detail_c,
        (SELECT COUNT(*) FROM awx x
            WHERE x.detail = 'armed_entry_submit_timeout'
              AND EXISTS (
                  SELECT 1
                  FROM awe e
                  WHERE e.signal_id = x.signal_id
                    AND e.enter_at < x.created_at
              )) AS enter_preceded_c,
        (SELECT COUNT(*) FROM awx x
            WHERE x.detail = 'armed_entry_submit_timeout'
              AND NOT EXISTS (
                  SELECT 1
                  FROM ele l
                  WHERE l.signal_id = x.signal_id
              )) AS missing_event_c,
        (SELECT COUNT(*) FROM awx x
            WHERE x.detail = 'armed_entry_submit_timeout'
              AND EXISTS (
                  SELECT 1
                  FROM ele l
                  WHERE l.signal_id = x.signal_id
              )
              AND NOT EXISTS (
                  SELECT 1
                  FROM ele l
                  WHERE l.signal_id = x.signal_id
                    AND COALESCE(l.detail,'') = COALESCE(x.detail,'')
              )) AS mismatch_c
)
SELECT
    exit_c,
    detail_c,
    enter_preceded_c,
    missing_event_c,
    mismatch_c,
    CASE
        WHEN exit_c = 0 AND detail_c = 0 THEN 'NA'
        WHEN detail_c >= 1
         AND enter_preceded_c = detail_c
         AND missing_event_c = 0
         AND mismatch_c = 0 THEN 'PASS'
        ELSE 'FAIL'
    END AS verdict
FROM v;
