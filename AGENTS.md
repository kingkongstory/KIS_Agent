# Repository Guidelines

## 시스템 목적
이 저장소는 한국투자증권 Open Trading API 기반 자동매매 및 모니터링 시스템이다. 핵심 전략은 09:00부터 15분 동안 형성된 15분봉의 고가·저가를 기준으로 당일 기준 구간을 만든 뒤, 이후 5분봉으로 내려가 진입 신호를 찾는 것이다. 상승형 캔들이 기준 고가를 몸통으로 돌파하면서 동시에 FVG가 생성되면 유효 신호로 본다. 이후 가격이 FVG 구간으로 재진입하면 매수 진입을 검토하고, 손절은 FVG 형성 캔들의 직전 캔들 저가에 둔다. 익절은 기본적으로 손익비 `1:2` 또는 `1:3`을 기준으로 관리한다. 구현 변경 시 이 흐름이 백테스트와 라이브에서 일관되게 유지되는지 먼저 확인한다.

## 프로젝트 구조
백엔드는 `src/` 아래에 있으며 `domain/`, `application/`, `infrastructure/`, `presentation/`, `strategy/` 계층으로 나뉜다. 진입점은 `src/main.rs`, 공용 모듈 노출은 `src/lib.rs`가 맡는다. 프론트엔드는 `frontend/src/`를 기준으로 `components/`, `pages/`, `api/`, `stores/`, `hooks/`, `styles/`로 구성된다. 참고 문서는 `docs/`에 모아두며, `frontend/dist/`, `target/`, `logs/`, `kis_cache.db`는 생성 산출물이므로 직접 수정하지 않는다.

## 빌드·테스트·개발 명령
- `cargo run` : `.env` 설정을 읽어 Axum 서버를 실행한다.
- `cargo run -- backtest 122630 --days 5 --multi-stage` : 대표 백테스트를 실행한다.
- `cargo test` : Rust 단위 테스트와 비동기 테스트를 함께 실행한다.
- `cargo fmt -- --check` / `cargo clippy` : 포맷과 린트를 점검한다.
- `cd frontend && npm install` : 프론트엔드 의존성을 설치한다.
- `cd frontend && npm run dev` : Vite 개발 서버를 실행한다.
- `cd frontend && npm run build` : `tsc -b` 후 `frontend/dist/`를 생성한다.

## 코딩 스타일 및 네이밍
Rust는 `rustfmt` 기본 형식을 따르며 들여쓰기는 4칸, 모듈·함수는 `snake_case`, 타입은 `PascalCase`를 사용한다. 포트 인터페이스는 `src/domain/ports/`의 trait 패턴을 유지한다. TypeScript는 strict 모드 기준이며 들여쓰기는 2칸, 컴포넌트 파일은 `DashboardPage.tsx`처럼 `PascalCase`, 훅과 스토어는 `useStockStore`처럼 `camelCase`를 사용한다. 코드 식별자는 영문으로 두고, 커밋 제목과 운영 설명, 주석은 한국어를 우선한다.

## 테스트 지침
테스트는 가능한 한 구현 파일 가까이에 `#[test]` 또는 `#[tokio::test]`로 둔다. 전략 회귀 검증은 `src/strategy/parity/tests.rs`와 각 전략 모듈의 인라인 테스트를 우선 활용한다. PR 전에는 최소 `cargo test`를 실행하고, UI 변경은 별도 테스트 러너가 없으므로 `npm run build` 통과 여부와 수동 확인 결과를 함께 남긴다.

## 커밋 및 PR 지침
최근 이력은 `문서: ...`, `핫픽스: ...`처럼 짧은 한국어 제목과 선택적 범주 접두사를 사용한다. 같은 형식을 유지하고 제목에는 변경 이유와 핵심 결과를 함께 적는다. PR에는 변경 요약, 영향 모듈, 설정 변경 여부, 검증 명령, 프론트엔드 변경 시 화면 캡처를 포함한다. 매매 로직 수정이면 관련 `docs/monitoring/` 문서나 사고 분석 문서를 함께 연결한다.

## 보안 및 설정 주의
`.env.example`을 복사해 `.env`를 만들고 실제 KIS 자격 증명은 커밋하지 않는다. 특별한 이유가 없으면 `KIS_ENVIRONMENT=paper`를 기본으로 사용한다. 라이브 실행 전에는 `DATABASE_URL`, 자동 시작 옵션, 스케줄러 설정이 실제 운용 의도와 일치하는지 다시 확인한다.
