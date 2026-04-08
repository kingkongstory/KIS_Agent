//! KIS API는 숫자 필드를 문자열로 반환하므로, 커스텀 역직렬화 모듈이 필요하다.

/// 문자열 → i64 역직렬화
pub mod string_to_i64 {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn deserialize<'de, D>(deserializer: D) -> Result<i64, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        let s = s.trim();
        if s.is_empty() {
            return Ok(0);
        }
        s.parse::<i64>().map_err(serde::de::Error::custom)
    }

    pub fn serialize<S>(value: &i64, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&value.to_string())
    }
}

/// 문자열 → f64 역직렬화
pub mod string_to_f64 {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn deserialize<'de, D>(deserializer: D) -> Result<f64, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        let s = s.trim();
        if s.is_empty() {
            return Ok(0.0);
        }
        s.parse::<f64>().map_err(serde::de::Error::custom)
    }

    pub fn serialize<S>(value: &f64, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&value.to_string())
    }
}

/// 문자열 → Option<i64> 역직렬화 (빈 문자열은 None)
pub mod optional_string_to_i64 {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<i64>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        let s = s.trim();
        if s.is_empty() {
            return Ok(None);
        }
        s.parse::<i64>().map(Some).map_err(serde::de::Error::custom)
    }

    pub fn serialize<S>(value: &Option<i64>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match value {
            Some(v) => serializer.serialize_str(&v.to_string()),
            None => serializer.serialize_str(""),
        }
    }
}

/// "YYYYMMDD" → NaiveDate 역직렬화
pub mod kis_date {
    use chrono::NaiveDate;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn deserialize<'de, D>(deserializer: D) -> Result<NaiveDate, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        NaiveDate::parse_from_str(s.trim(), "%Y%m%d").map_err(serde::de::Error::custom)
    }

    pub fn serialize<S>(date: &NaiveDate, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&date.format("%Y%m%d").to_string())
    }
}

/// "HHMMSS" → NaiveTime 역직렬화
pub mod kis_time {
    use chrono::NaiveTime;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn deserialize<'de, D>(deserializer: D) -> Result<NaiveTime, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        NaiveTime::parse_from_str(s.trim(), "%H%M%S").map_err(serde::de::Error::custom)
    }

    pub fn serialize<S>(time: &NaiveTime, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&time.format("%H%M%S").to_string())
    }
}

/// 선택적 "YYYYMMDD" → Option<NaiveDate> 역직렬화
pub mod optional_kis_date {
    use chrono::NaiveDate;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<NaiveDate>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        let s = s.trim();
        if s.is_empty() {
            return Ok(None);
        }
        NaiveDate::parse_from_str(s, "%Y%m%d")
            .map(Some)
            .map_err(serde::de::Error::custom)
    }

    pub fn serialize<S>(date: &Option<NaiveDate>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match date {
            Some(d) => serializer.serialize_str(&d.format("%Y%m%d").to_string()),
            None => serializer.serialize_str(""),
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::{NaiveDate, NaiveTime};
    use serde::Deserialize;

    #[derive(Deserialize)]
    struct TestI64 {
        #[serde(deserialize_with = "super::string_to_i64::deserialize")]
        value: i64,
    }

    #[derive(Deserialize)]
    struct TestF64 {
        #[serde(deserialize_with = "super::string_to_f64::deserialize")]
        value: f64,
    }

    #[derive(Deserialize)]
    struct TestOptI64 {
        #[serde(deserialize_with = "super::optional_string_to_i64::deserialize")]
        value: Option<i64>,
    }

    #[derive(Deserialize)]
    struct TestDate {
        #[serde(deserialize_with = "super::kis_date::deserialize")]
        value: NaiveDate,
    }

    #[derive(Deserialize)]
    struct TestTime {
        #[serde(deserialize_with = "super::kis_time::deserialize")]
        value: NaiveTime,
    }

    #[test]
    fn test_string_to_i64() {
        let json = r#"{"value": "12345"}"#;
        let parsed: TestI64 = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.value, 12345);
    }

    #[test]
    fn test_string_to_i64_negative() {
        let json = r#"{"value": "-500"}"#;
        let parsed: TestI64 = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.value, -500);
    }

    #[test]
    fn test_string_to_i64_empty() {
        let json = r#"{"value": ""}"#;
        let parsed: TestI64 = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.value, 0);
    }

    #[test]
    fn test_string_to_f64() {
        let json = r#"{"value": "3.14"}"#;
        let parsed: TestF64 = serde_json::from_str(json).unwrap();
        assert!((parsed.value - 3.14).abs() < f64::EPSILON);
    }

    #[test]
    fn test_optional_string_to_i64_some() {
        let json = r#"{"value": "100"}"#;
        let parsed: TestOptI64 = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.value, Some(100));
    }

    #[test]
    fn test_optional_string_to_i64_none() {
        let json = r#"{"value": ""}"#;
        let parsed: TestOptI64 = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.value, None);
    }

    #[test]
    fn test_kis_date() {
        let json = r#"{"value": "20250115"}"#;
        let parsed: TestDate = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.value, NaiveDate::from_ymd_opt(2025, 1, 15).unwrap());
    }

    #[test]
    fn test_kis_time() {
        let json = r#"{"value": "093015"}"#;
        let parsed: TestTime = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.value, NaiveTime::from_hms_opt(9, 30, 15).unwrap());
    }
}
