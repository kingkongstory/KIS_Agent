/// 캐시 포트 — application 계층에서 캐시 구현체에 의존하지 않도록 추상화
///
/// Send + Sync 필요: 비동기 서비스에서 Arc로 공유됨
pub trait CachePort<V: Clone>: Send + Sync {
    /// 캐시에서 값 가져오기 (만료 시 None)
    fn get(&self, key: &str) -> Option<V>;
    /// 캐시에 값 저장 (기본 TTL)
    fn set(&self, key: String, value: V);
}
