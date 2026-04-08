# 네이버 금융 API — 일봉 데이터 수집 레퍼런스

## 개요

네이버 금융의 비공식 차트 API. 인증 없이 일봉/주봉/월봉 OHLCV 데이터를 수 년치 즉시 조회 가능.
분봉은 종가만 제공(OHLC 불완전)하므로 **일봉 + 지수 데이터 수집용**으로 활용한다.

## 엔드포인트

```
GET https://fchart.stock.naver.com/siseJson.naver
```

대체 도메인 (동일 응답):
```
GET https://api.finance.naver.com/siseJson.naver
```

## 요청 파라미터

| 파라미터 | 필수 | 값 | 설명 |
|---|---|---|---|
| symbol | O | `005930`, `KOSPI`, `KOSDAQ` | 종목코드 6자리 또는 지수명 |
| requestType | O | `1` | 고정값 (2는 빈 응답) |
| startTime | O | `20230101` | 시작일 (YYYYMMDD) |
| endTime | O | `20260408` | 종료일 (YYYYMMDD) |
| timeframe | O | `day`, `week`, `month`, `minute` | 시간단위 |

### 예시 URL

```
# 삼성전자 일봉 (2023~2026)
https://fchart.stock.naver.com/siseJson.naver?symbol=005930&requestType=1&startTime=20230101&endTime=20260408&timeframe=day

# 코스피 지수 일봉
https://fchart.stock.naver.com/siseJson.naver?symbol=KOSPI&requestType=1&startTime=20230101&endTime=20260408&timeframe=day

# 코스닥 지수 일봉
https://fchart.stock.naver.com/siseJson.naver?symbol=KOSDAQ&requestType=1&startTime=20230101&endTime=20260408&timeframe=day
```

## 응답 형식

**주의: 표준 JSON이 아닌 "유사 JSON"**

- 헤더 행: 작은따옴표(`'`)로 감싸진 필드명
- 데이터 행: 큰따옴표(`"`) 날짜 + 숫자
- 파싱 방법: 작은따옴표 → 큰따옴표 치환 후 JSON 파싱

### 원본 응답 예시

```
[['날짜', '시가', '고가', '저가', '종가', '거래량', '외국인소진율'],
["20260407", 56200, 57400, 55900, 57100, 18651234, 55.2],
["20260408", 57100, 57800, 56500, 57500, 15423567, 55.3]
]
```

### 필드 (7개 컬럼)

| 인덱스 | 필드 | 타입 | 일봉 | 분봉 |
|---|---|---|---|---|
| 0 | 날짜 | string | YYYYMMDD | YYYYMMDDHHmm |
| 1 | 시가 | number | O | **null** |
| 2 | 고가 | number | O | **null** |
| 3 | 저가 | number | O | **null** |
| 4 | 종가 | number | O | O (체결가) |
| 5 | 거래량 | number | O | O (누적) |
| 6 | 외국인소진율 | number | O | **null** |

## 심볼별 지원 현황

| 심볼 | 타입 | 일봉 | 분봉 |
|---|---|---|---|
| `005930` | 종목 (삼성전자) | O | 제한적 (종가만) |
| `KOSPI` | 코스피 지수 | O | **미지원** (빈 응답) |
| `KOSDAQ` | 코스닥 지수 | O | **미지원** |
| `KS11` | 코스피 (대체) | **미지원** | - |

## 제한사항

- **비공식 API**: 사전 통보 없이 변경/차단 가능
- **CORS 미지원**: 브라우저에서 직접 호출 불가 → 서버 프록시 필요
- **인증 불필요**: API 키 없이 호출 가능
- **속도 제한**: 명시적 제한 없으나 과도한 요청 시 IP 차단 가능
- **분봉 제한**: OHLC 중 종가(체결가)만 제공 → 캔들 기반 지표 계산 불가
- **수정주가 기준**: 액면분할 등 반영된 가격

## 활용 전략

| 용도 | 적합성 | 비고 |
|---|---|---|
| 일봉 OHLCV 대량 수집 | **최적** | 수 년치 즉시 조회, 무인증 |
| 코스피/코스닥 지수 일봉 | **최적** | KOSPI, KOSDAQ 심볼 |
| 분봉 백테스팅 | **부적합** | 시가/고가/저가 없음 |

## Rust 파싱 참조

```rust
// 응답 전처리: 작은따옴표 → 큰따옴표, 공백 제거
fn parse_naver_response(raw: &str) -> Vec<Vec<serde_json::Value>> {
    let cleaned = raw
        .replace('\'', "\"")
        .lines()
        .map(|l| l.trim())
        .collect::<String>();
    serde_json::from_str(&cleaned).unwrap_or_default()
}
```
