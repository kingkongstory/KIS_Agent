// 2026-04-18 Review round (Finding: clippy 46건) — crate-level allow.
//
// 다음 lint 들은 도메인 특성상 발생하며 수정 대비 이득이 작다:
// - `clippy::too_many_arguments`: SQL 바인딩 helper / PositionManager 업데이트 등에서
//   파라미터가 많은 것이 가독성상 더 나음 (builder 패턴 도입은 오히려 과잉 추상화).
// - `clippy::type_complexity`: HashMap/Arc/RwLock 중첩이 실시간 상태 공유 구조라 필연적.
//   type alias 로 숨기면 의미가 분산되어 읽기 어려움.
//
// 그 외 lint (unused_variables, dead_code 등) 는 개별 `#[allow]` 로 처리한다.
#![allow(clippy::too_many_arguments)]
#![allow(clippy::type_complexity)]

pub mod application;
pub mod config;
pub mod domain;
pub mod infrastructure;
pub mod presentation;
pub mod strategy;
