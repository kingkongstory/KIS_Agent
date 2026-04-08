> [docs/README.md](../README.md) > [api/](./overview.md) > authentication.md

# 인증

KIS API는 OAuth 2.0 기반 인증을 사용한다. 토큰 발급 → 요청에 포함 → 자동 갱신의 흐름이다.

## 사전 준비

1. [KIS 개발자 포털](https://apiportal.koreainvestment.com)에서 회원가입
2. 앱 등록 후 `appkey`와 `appsecret` 발급
3. 실전투자와 모의투자는 **별도 앱키**가 필요

> **보안**: `appkey`와 `appsecret`은 환경변수 또는 `.env` 파일로 관리. 절대 코드에 하드코딩하지 않는다.

## REST API 토큰 발급

### 엔드포인트

```
POST /oauth2/tokenP
```

### 요청

```json
{
  "grant_type": "client_credentials",
  "appkey": "PSxxxxxxxxxxxxxxxxxx",
  "appsecret": "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"
}
```

| 필드 | 타입 | 필수 | 설명 |
|------|------|------|------|
| `grant_type` | String | O | 고정값: `"client_credentials"` |
| `appkey` | String | O | 앱키 |
| `appsecret` | String | O | 앱시크릿 |

### 응답

```json
{
  "access_token": "eyJ0eXAi...",
  "access_token_token_expired": "2026-04-09 12:00:00",
  "token_type": "Bearer",
  "expires_in": 86400
}
```

| 필드 | 타입 | 설명 |
|------|------|------|
| `access_token` | String | Bearer 인증 토큰 |
| `access_token_token_expired` | String | 만료 시각 (형식: `YYYY-MM-DD HH:MM:SS`) |
| `token_type` | String | `"Bearer"` 고정 |
| `expires_in` | Number | 만료까지 남은 초 (약 86400 = 24시간) |

### Rust 구조체

```rust
#[derive(Serialize)]
struct TokenRequest {
    grant_type: String, // "client_credentials"
    appkey: String,
    appsecret: String,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    access_token_token_expired: String,
    token_type: String,
    expires_in: u64,
}
```

## 토큰 생명주기

### 핵심 규칙

| 규칙 | 설명 |
|------|------|
| 유효기간 | 약 **24시간** (발급 시점 기준) |
| 재발급 제한 | **분당 최대 1회** |
| 6시간 규칙 | 발급 후 6시간 이내 재발급 시 **동일 토큰** 반환 |
| 동시 토큰 | 1개의 앱키에 1개의 유효 토큰만 존재 |

### 자동 갱신 설계

토큰 만료로 인한 API 호출 실패를 방지하려면 중앙 토큰 관리자가 필요하다:

```rust
use tokio::sync::RwLock;
use std::sync::Arc;

struct TokenManager {
    token: Arc<RwLock<Option<TokenInfo>>>,
    appkey: String,
    appsecret: String,
    environment: Environment,
}

struct TokenInfo {
    access_token: String,
    expires_at: DateTime<Utc>,
}

impl TokenManager {
    /// 유효한 토큰을 반환. 만료 임박 시 자동 갱신.
    async fn get_token(&self) -> Result<String, KisError> {
        let token = self.token.read().await;
        if let Some(info) = token.as_ref() {
            // 만료 30분 전까지는 기존 토큰 사용
            if info.expires_at - Duration::minutes(30) > Utc::now() {
                return Ok(info.access_token.clone());
            }
        }
        drop(token);
        self.refresh_token().await
    }

    /// 토큰 갱신 (분당 1회 제한 준수)
    async fn refresh_token(&self) -> Result<String, KisError> {
        let mut token = self.token.write().await;
        // 다른 태스크가 이미 갱신했는지 재확인 (double-check)
        if let Some(info) = token.as_ref() {
            if info.expires_at - Duration::minutes(30) > Utc::now() {
                return Ok(info.access_token.clone());
            }
        }
        // 토큰 발급 API 호출
        let response = self.request_new_token().await?;
        let info = TokenInfo {
            access_token: response.access_token.clone(),
            expires_at: parse_kis_datetime(&response.access_token_token_expired)?,
        };
        *token = Some(info);
        Ok(response.access_token)
    }
}
```

**설계 포인트**:
- `RwLock`으로 읽기는 동시, 쓰기(갱신)는 배타적 접근
- Double-check 패턴으로 불필요한 중복 갱신 방지
- 만료 30분 전에 미리 갱신하여 여유 확보

## WebSocket 접속키 (approval_key)

REST 토큰과 별도로, WebSocket 연결에는 `approval_key`가 필요하다.

### 엔드포인트

```
POST /oauth2/Approval
```

### 요청

```json
{
  "grant_type": "client_credentials",
  "appkey": "PSxxxxxxxxxxxxxxxxxx",
  "appsecret": "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"
}
```

### 응답

```json
{
  "approval_key": "xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx"
}
```

| 필드 | 타입 | 설명 |
|------|------|------|
| `approval_key` | String | WebSocket 접속키 (UUID 형식) |

### REST 토큰과의 차이

| 항목 | REST 토큰 | WebSocket 접속키 |
|------|----------|-----------------|
| 용도 | REST API 요청 인증 | WebSocket 연결 인증 |
| 발급 경로 | `/oauth2/tokenP` | `/oauth2/Approval` |
| 형식 | JWT (긴 문자열) | UUID |
| 사용 위치 | HTTP 헤더 (`Authorization`) | WebSocket 연결 메시지 내부 |

## 실전/모의투자 차이

| 항목 | 실전투자 | 모의투자 |
|------|---------|---------|
| 토큰 발급 URL | `https://openapi.koreainvestment.com:9443/oauth2/tokenP` | `https://openapivts.koreainvestment.com:29443/oauth2/tokenP` |
| 앱키 | 실전용 별도 발급 | 모의용 별도 발급 |
| 토큰 유효기간 | 24시간 | 24시간 |

> **주의**: 실전/모의투자 앱키는 교차 사용할 수 없다.

## 관련 문서

- [API 공통사항](overview.md) — 공통 헤더에 토큰 포함 방법
- [WebSocket](websocket.md) — approval_key 사용
- [속도 제한](rate-limits.md) — 토큰 발급 분당 1회 제한
