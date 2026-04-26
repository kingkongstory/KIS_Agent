> [docs/README.md](../README.md) > [api/](./overview.md) > websocket.md

# WebSocket 실시간 데이터

REST API는 요청-응답 방식이므로 실시간 시세에는 WebSocket을 사용한다.

## REST vs WebSocket

| 항목 | REST API | WebSocket |
|------|----------|-----------|
| 방식 | 요청 → 응답 (폴링) | 구독 → 실시간 수신 |
| 용도 | 시세 조회, 주문, 잔고 | 실시간 체결가, 호가, 체결통보 |
| 인증 | Bearer 토큰 | approval_key |
| 데이터 형식 | JSON | 헤더 + 파이프(|) 구분 데이터 |

## 연결 설정

### 접속 URL

| 환경 | URL |
|------|-----|
| 실전투자 | `ws://ops.koreainvestment.com:21000` |
| 모의투자 | `ws://ops.koreainvestment.com:31000` |

### 인증

WebSocket 연결에는 `approval_key`가 필요하다 ([인증](authentication.md) 참조).

### 구독 요청 메시지

```json
{
  "header": {
    "approval_key": "xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx",
    "custtype": "P",
    "tr_type": "1",
    "content-type": "utf-8"
  },
  "body": {
    "input": {
      "tr_id": "H0STCNT0",
      "tr_key": "005930"
    }
  }
}
```

| 필드 | 설명 |
|------|------|
| `tr_type` | `"1"` = 구독, `"2"` = 구독해제 |
| `tr_id` | 구독 유형 (아래 표 참조) |
| `tr_key` | 종목코드 |

## 구독 유형

### 실시간 체결가 (H0STCNT0)

주식의 실시간 체결 데이터를 수신한다.

**tr_id**: `H0STCNT0`

아래 순서는 KIS 공식 GitHub 샘플 `legacy/Sample01/kis_domstk_ws.py` 의
`contract_cols` 0-based 순서를 기준으로 한다.

**수신 데이터 주요 필드**:

| 순서 | 필드명 | 설명 |
|------|--------|------|
| 0 | 유가증권단축종목코드 | 종목코드 |
| 1 | 주식체결시간 | HHMMSS |
| 2 | 주식현재가 | 현재 체결가 |
| 3 | 전일대비부호 | 1~5 |
| 4 | 전일대비 | |
| 5 | 전일대비율 | % |
| 6 | 가중평균주식가격 | |
| 7 | 시가 | |
| 8 | 고가 | |
| 9 | 저가 | |
| 10 | 매도호가1 | |
| 11 | 매수호가1 | |
| 12 | 체결거래량 | 직전 체결 수량 |
| 13 | 누적거래량 | |
| 14 | 누적거래대금 | |

> 나머지 후속 필드도 공식 샘플의 `contract_cols` 순서를 따른다.

### 실시간 호가 (H0STASP0)

매도/매수 10단계 호가의 실시간 변동을 수신한다.

**tr_id**: `H0STASP0`

아래 순서는 KIS 공식 GitHub 샘플 `legacy/Sample01/kis_domstk_ws.py` 의
`bid_ask_cols` 0-based 순서를 기준으로 한다.

**수신 데이터 주요 필드**:

| 순서 | 필드명 | 설명 |
|------|--------|------|
| 0 | 종목코드 | |
| 3 | 매도호가1 ~ 매도호가10 | 3~12 |
| 13 | 매수호가1 ~ 매수호가10 | 13~22 |
| 23 | 매도호가잔량1 ~ 10 | 23~32 |
| 33 | 매수호가잔량1 ~ 10 | 33~42 |
| 43 | 총매도호가잔량 | |
| 44 | 총매수호가잔량 | |

### 체결 통보 (H0STCNI0 / H0STCNI9)

**내 주문**의 체결/미체결 상태를 실시간으로 수신한다.

**tr_id**: `H0STCNI0` (실전), `H0STCNI9` (모의)

> **주의**: 체결 통보 데이터는 **AES-256-CBC로 암호화**되어 전송된다. 복호화가 필요하다.

```rust
use aes::Aes256;
use cbc::{Cipher, Decryptor};

/// KIS WebSocket 체결 통보 복호화
fn decrypt_execution_notice(
    encrypted_data: &[u8],
    key: &[u8; 32],  // approval_key에서 파생
    iv: &[u8; 16],
) -> Result<String, KisError> {
    // AES-256-CBC 복호화
    // ...
}
```

## 데이터 파싱

### 메시지 구조

WebSocket 수신 메시지는 크게 두 가지 형식이다:

**1. 제어 메시지 (JSON)**
```json
{
  "header": {
    "tr_id": "H0STCNT0",
    "tr_key": "005930",
    "encrypt": "N"
  },
  "body": {
    "rt_cd": "0",
    "msg_cd": "OPSP0000",
    "msg1": "SUBSCRIBE SUCCESS"
  }
}
```

**2. 데이터 메시지 (파이프 구분)**
```
0|H0STCNT0|001|005930^103025^72500^5^500^0.69^...
```

구조: `암호화여부|tr_id|데이터건수|데이터본문`

- 암호화여부: `0` = 평문, `1` = 암호화
- 데이터본문: `^` 또는 `|`로 필드 구분

### Rust 파싱

```rust
/// WebSocket 메시지 종류
enum WsMessage {
    /// 구독 확인 등 제어 메시지
    Control(ControlMessage),
    /// 실시간 시세 데이터
    Data(DataMessage),
}

/// 데이터 메시지 파싱
struct DataMessage {
    encrypted: bool,
    tr_id: String,
    count: usize,
    records: Vec<Vec<String>>,
}

fn parse_ws_message(raw: &str) -> Result<WsMessage, KisError> {
    // JSON인지 파이프 구분인지 판별
    if raw.starts_with('{') {
        let ctrl: ControlMessage = serde_json::from_str(raw)?;
        Ok(WsMessage::Control(ctrl))
    } else {
        let parts: Vec<&str> = raw.splitn(4, '|').collect();
        let encrypted = parts[0] == "1";
        let tr_id = parts[1].to_string();
        let count = parts[2].parse::<usize>()?;
        let body = parts[3];
        let records: Vec<Vec<String>> = body
            .split('^')
            .collect::<Vec<&str>>()
            .chunks(/* 필드 수 */)
            .map(|chunk| chunk.iter().map(|s| s.to_string()).collect())
            .collect();
        Ok(WsMessage::Data(DataMessage {
            encrypted, tr_id, count, records,
        }))
    }
}
```

## 재연결 전략

KIS WebSocket은 "No close frame received" 오류로 예고 없이 끊어질 수 있다. 견고한 재연결 로직이 필수.

### 지수 백오프 재연결

```rust
async fn connect_with_retry(config: &WsConfig) -> Result<WsConnection, KisError> {
    let mut attempt = 0;
    let max_attempts = 10;
    let base_delay = Duration::from_secs(1);
    let max_delay = Duration::from_secs(60);

    loop {
        match try_connect(config).await {
            Ok(conn) => {
                if attempt > 0 {
                    log::info!("WebSocket 재연결 성공 (시도 {}회)", attempt + 1);
                }
                return Ok(conn);
            }
            Err(e) => {
                attempt += 1;
                if attempt >= max_attempts {
                    return Err(KisError::WebSocketError(
                        format!("{}회 시도 후 연결 실패", max_attempts)
                    ));
                }
                let delay = std::cmp::min(
                    base_delay * 2u32.pow(attempt as u32 - 1),
                    max_delay,
                );
                log::warn!(
                    "WebSocket 연결 실패 ({}회차): {}. {}초 후 재시도",
                    attempt, e, delay.as_secs()
                );
                tokio::time::sleep(delay).await;
            }
        }
    }
}
```

### 구독 상태 복원

재연결 후 기존 구독을 자동으로 복원해야 한다:

```rust
struct SubscriptionManager {
    /// 현재 구독 중인 종목 목록
    subscriptions: HashMap<String, Vec<String>>, // tr_id -> [종목코드]
}

impl SubscriptionManager {
    /// 재연결 후 모든 구독 복원
    async fn restore_all(&self, ws: &mut WsConnection) -> Result<(), KisError> {
        for (tr_id, stock_codes) in &self.subscriptions {
            for code in stock_codes {
                ws.subscribe(tr_id, code).await?;
                // 구독 요청 간 약간의 간격
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
        Ok(())
    }
}
```

### Heartbeat 관리

연결 상태를 주기적으로 확인:
- WebSocket ping/pong 프레임 활용
- 일정 시간(예: 30초) 데이터 수신 없으면 연결 상태 확인
- 응답 없으면 재연결 시작

## Rust 구현 가이드: 전체 아키텍처

```rust
/// WebSocket 클라이언트 구조
struct KisWebSocket {
    config: WsConfig,
    subscriptions: Arc<RwLock<SubscriptionManager>>,
    /// 수신 데이터를 소비자에게 전달하는 채널
    data_tx: mpsc::Sender<RealtimeData>,
}

/// 실시간 데이터 타입
enum RealtimeData {
    /// 체결가 업데이트
    Execution {
        stock_code: String,
        price: i64,
        volume: u64,
        time: String,
    },
    /// 호가 업데이트
    OrderBook {
        stock_code: String,
        asks: Vec<(i64, u64)>,  // (가격, 잔량)
        bids: Vec<(i64, u64)>,
    },
    /// 내 주문 체결 통보
    ExecutionNotice {
        order_no: String,
        stock_code: String,
        side: OrderSide,
        price: i64,
        quantity: u64,
    },
}
```

- `tokio-tungstenite` 크레이트로 WebSocket 연결
- `mpsc` 채널로 수신 데이터를 다른 모듈(지표 엔진 등)에 전달
- 구독 관리와 재연결을 별도 태스크로 분리

## 관련 문서

- [인증](authentication.md) — approval_key 발급
- [국내주식 시세](domestic-stock-quotations.md) — REST 방식 시세 조회
- [기술적 분석 개요](../indicators/overview.md) — 실시간 데이터 → 지표 계산
