# LS증권 Open API — 분봉 데이터 수집 레퍼런스

## 개요

LS증권(구 이베스트투자증권) Open API는 REST/WebSocket 기반으로 주식 시세, 분봉 차트, 주문 등을 제공한다.
**분봉 OHLCV 데이터를 REST API로 조회할 수 있는 유일한 국내 증권사**이며, 백테스팅용 과거 데이터 수집에 활용한다.

## 기본 정보

| 항목 | 값 |
|---|---|
| 포털 | https://openapi.ls-sec.co.kr |
| REST 기본 URL (실전) | `https://openapi.ls-sec.co.kr:8080` |
| WebSocket (실전) | `wss://openapi.ls-sec.co.kr:9443/websocket` |
| 인증 방식 | OAuth 2.0 (client_credentials) |
| 비용 | 무료 (데이터 조회) |
| 계좌 요건 | LS증권 비대면 계좌 개설, xingAPI 사용등록, Open API 신청 |
| 최대 API 계정 | 3개 |
| 모의투자 | 지원 (별도 키로 자동 라우팅) |

## 가입 절차

```
1. LS증권 홈페이지(ls-sec.co.kr)에서 비대면 계좌 개설
2. xingAPI 사용등록
3. Open API 사용 신청
4. APP Key / APP Secret 발급
```

- 최소 예치금 없음
- 비대면으로 전체 프로세스 완료 가능

---

## 인증

### 토큰 발급

```
POST /oauth2/token
Content-Type: application/x-www-form-urlencoded

grant_type=client_credentials
&appkey={APP_KEY}
&appsecretkey={APP_SECRET}
&scope=oob
```

**응답:**
```json
{
  "access_token": "eyJ...",
  "token_type": "Bearer",
  "expires_in": 7776000,
  "scope": "oob"
}
```

- 발급된 `access_token`을 `Authorization: Bearer {token}` 헤더에 사용
- 토큰 유효기간: 약 90일

---

## 핵심 API

### 1. t8412 — 주식 분봉 차트 (N분)

**우리 프로젝트의 주력 API.** 개별 종목의 과거 분봉 OHLCV를 조회한다.

```
POST /stock/chart
```

#### 요청 헤더

| 헤더 | 값 | 설명 |
|---|---|---|
| Content-Type | `application/json; charset=utf-8` | |
| authorization | `Bearer {ACCESS_TOKEN}` | OAuth 토큰 |
| tr_cd | `t8412` | TR 코드 |
| tr_cont | `N` / `Y` | 최초 조회: N, 연속 조회: Y |
| tr_cont_key | `""` | 빈 문자열 |

#### 요청 바디 (t8412InBlock)

```json
{
  "t8412InBlock": {
    "shcode": "005930",
    "ncnt": 1,
    "qrycnt": 2000,
    "nday": "1",
    "sdate": "20250401",
    "stime": "090000",
    "edate": "20260408",
    "etime": "153000",
    "cts_date": "",
    "cts_time": "",
    "comp_yn": "Y"
  }
}
```

| 필드 | 타입 | 필수 | 설명 |
|---|---|---|---|
| shcode | string | O | 종목코드 (6자리) |
| ncnt | int | O | N분 단위 (1, 3, 5, 10, 15, 30, 60) |
| qrycnt | int | O | 조회 건수 (최대 2000) |
| nday | string | | 날짜 구분 |
| sdate | string | O | 시작일 (YYYYMMDD) |
| stime | string | | 시작시간 (HHMMSS) |
| edate | string | O | 종료일 (YYYYMMDD) |
| etime | string | | 종료시간 (HHMMSS) |
| cts_date | string | | 연속조회 날짜 키 (이전 응답값) |
| cts_time | string | | 연속조회 시간 키 (이전 응답값) |
| comp_yn | string | | 압축 여부 (Y/N) |

#### 응답

**t8412OutBlock** (메타):

| 필드 | 설명 |
|---|---|
| cts_date | 연속조회 날짜 키 |
| cts_time | 연속조회 시간 키 |

**t8412OutBlock1** (분봉 데이터 배열):

| 필드 | 타입 | 설명 |
|---|---|---|
| date | string | 거래일자 (YYYYMMDD) |
| time | string | 거래시간 (HHMMSS) |
| open | int | **시가** |
| high | int | **고가** |
| low | int | **저가** |
| close | int | **종가** |
| jdiff_vol | int | **거래량** |
| value | int | 거래대금 |
| rate | float | 등락률 |
| sign | string | 전일 대비 부호 |

#### 연속조회 (페이지네이션)

```
1회차: tr_cont="N", cts_date="", cts_time="" → 최신 2000건
2회차: tr_cont="Y", cts_date=응답값, cts_time=응답값 → 다음 2000건
...반복 (데이터 없으면 종료)
```

#### 제한사항

- 속도 제한: **초당 1건**
- 과거 범위: 1분봉 기준 **약 1년**
- 5분봉 이상은 더 과거까지 조회 가능 (정확한 기간 미공개, 실측 필요)

#### 수집 소요 시간 추정

| 대상 | 1분봉 건수 (1년) | 조회 횟수 | 소요 시간 |
|---|---|---|---|
| 1종목 | ~95,000 | ~48회 | ~1분 |
| 10종목 | ~950,000 | ~480회 | ~8분 |
| 100종목 | ~9,500,000 | ~4,800회 | ~80분 |

---

### 2. t8418 — 업종/지수 분봉 차트 (N분)

코스피/코스닥 지수의 분봉 데이터를 조회한다.

```
POST /indtp/chart
```

#### 요청 헤더

t8412와 동일 구조. `tr_cd`만 `t8418`로 변경.

#### 요청 바디 (t8418InBlock)

t8412와 동일 구조. `shcode`에 **지수코드** 사용:

| 지수 | 코드 |
|---|---|
| 코스피 종합 | `001` |
| 코스닥 종합 | `301` |
| 코스피200 | `101` |

#### 응답 (t8418OutBlock1)

t8412와 동일한 OHLCV 필드 구조.

---

### 3. t8410 — 일봉/주봉/월봉 차트

```
POST /stock/chart
tr_cd: t8410
```

- 수정주가 옵션: `sujung` 필드 (`Y`로 설정 시 수정주가 반환)
- 수 년치 과거 데이터 조회 가능
- 응답 구조는 t8412와 유사

---

### 4. t8430 — 종목 마스터 (전체 종목 목록)

```
POST /stock/etc
tr_cd: t8430
```

- 코스피/코스닥/전체 구분 가능
- 종목코드, 종목명, 시장구분 등 반환

---

## Rust 구현 참조

### 아키텍처 배치

```
src/infrastructure/ls_client/
├── mod.rs
├── auth.rs          -- OAuth 토큰 관리 (LS증권용)
├── http_client.rs   -- LS증권 공통 HTTP 클라이언트
├── minute_chart.rs  -- t8412 분봉 조회 + 연속조회
├── index_chart.rs   -- t8418 지수 분봉 조회
├── daily_chart.rs   -- t8410 일봉 조회
└── stock_master.rs  -- t8430 종목 마스터
```

### 구현 시 주의사항

1. **속도 제한 준수**: t8412는 초당 1건. `tokio::time::sleep(Duration::from_secs(1))` 필수
2. **연속조회 루프**: `cts_date`가 빈 문자열이면 마지막 페이지
3. **토큰 갱신**: 유효기간 90일이지만, 만료 전 갱신 로직 구현
4. **데이터 정렬**: 응답 데이터가 최신→과거 순서일 수 있음. DB 저장 시 정렬 필요
5. **장 시간 데이터만**: 09:00~15:30 범위의 데이터만 유효. 장전/장후 데이터 필터링 고려
