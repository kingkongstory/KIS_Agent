> [docs/README.md](../README.md) > [api/](./overview.md) > rate-limits.md

# 속도 제한

## 제한 규칙

| 항목 | 제한 | 비고 |
|------|------|------|
| REST API 호출 | **초당 최대 15건** | 슬라이딩 윈도우 방식 |
| 신규 계정 (최초 3일) | **초당 최대 3건** | 3일 후 표준 할당 |
| 토큰 발급 | **분당 최대 1회** | 6시간 이내 재발급 시 동일 토큰 반환 |
| 모의투자 | 실전투자보다 **더 엄격** | 정확한 수치 미공개 |

### 슬라이딩 윈도우 방식

고정 시간 구간이 아닌, 임의의 1초 구간 내 요청 수를 카운트한다:

```
시간축: ──|──|──|──|──|──|──|──|──|──|──
요청:      ① ② ③ ④ ⑤         ⑥ ⑦
           ←── 1초 윈도우 ──→
```

윈도우가 매 요청마다 이동하므로, 단순히 "1초 시작 시점에 카운트 리셋"하는 방식으로는 정확히 대응할 수 없다.

## 제한 초과 시 동작

- HTTP 응답 코드: `200` (KIS는 HTTP 상태 코드로 에러를 구분하지 않는 경우가 있음)
- 응답 본문의 `rt_cd` ≠ `"0"`, `msg_cd`: `EGW00200`
- 제한 복구: 윈도우에서 오래된 요청이 빠져나갈 때까지 대기 (최대 1초)

## Rust 구현: 속도 제한기

### governor 크레이트 활용

```rust
use governor::{Quota, RateLimiter};
use std::num::NonZeroU32;

/// 환경별 속도 제한기 생성
fn create_rate_limiter(env: &Environment) -> RateLimiter</* ... */> {
    let quota = match env {
        Environment::Real => Quota::per_second(NonZeroU32::new(15).unwrap()),
        Environment::Paper => Quota::per_second(NonZeroU32::new(10).unwrap()), // 보수적 설정
    };
    RateLimiter::direct(quota)
}
```

### 요청 우선순위 처리

모든 요청이 동일한 제한을 공유하므로, 우선순위가 높은 요청이 밀리지 않도록 설계:

```rust
#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum RequestPriority {
    Critical = 0,  // 주문 실행/취소
    High = 1,      // 잔고 조회, 체결 확인
    Normal = 2,    // 시세 조회
    Low = 3,       // 통계, 차트 데이터
}
```

**우선순위 원칙**:
- **주문 관련** (매수/매도/취소): 최우선 — 지연되면 금전적 손실 가능
- **잔고/체결 확인**: 높음 — 주문 후 상태 확인에 필요
- **시세 조회**: 일반 — 약간의 지연 허용
- **차트/통계**: 낮음 — 배치성 데이터

### 토큰 발급 제한 (별도 관리)

토큰 발급은 REST API 속도 제한과 별개로, **분당 1회** 제한이 있다:

```rust
struct TokenRateLimiter {
    last_issued: Mutex<Option<Instant>>,
}

impl TokenRateLimiter {
    async fn wait_if_needed(&self) {
        let mut last = self.last_issued.lock().await;
        if let Some(last_time) = *last {
            let elapsed = last_time.elapsed();
            if elapsed < Duration::from_secs(60) {
                tokio::time::sleep(Duration::from_secs(60) - elapsed).await;
            }
        }
        *last = Some(Instant::now());
    }
}
```

## 대응 전략 요약

| 상황 | 전략 |
|------|------|
| 일반 API 호출 | governor로 초당 요청 수 제한 |
| 토큰 발급 | 별도 분당 1회 제한기 |
| 제한 초과 에러 수신 | 1초 대기 후 재시도 (최대 3회) |
| 대량 데이터 조회 (차트 등) | 요청 간 100ms 간격 삽입 |
| 모의투자 | 보수적 제한 (실전의 ~70%) |

## 관련 문서

- [API 공통사항](overview.md) — tr_id 체계
- [에러 처리](error-handling.md) — 속도 제한 에러 코드 (EGW00200)
- [인증](authentication.md) — 토큰 발급 제한
