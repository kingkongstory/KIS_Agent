use dashmap::DashMap;
use std::time::{Duration, Instant};

use crate::domain::ports::cache::CachePort;

/// TTL 기반 메모리 캐시
pub struct MemoryCache<V> {
    store: DashMap<String, CacheEntry<V>>,
    default_ttl: Duration,
}

struct CacheEntry<V> {
    value: V,
    expires_at: Instant,
}

impl<V: Clone> MemoryCache<V> {
    pub fn new(default_ttl: Duration) -> Self {
        Self {
            store: DashMap::new(),
            default_ttl,
        }
    }

    /// 캐시에서 값 가져오기 (만료 시 None)
    pub fn get(&self, key: &str) -> Option<V> {
        let entry = self.store.get(key)?;
        if Instant::now() < entry.expires_at {
            Some(entry.value.clone())
        } else {
            drop(entry);
            self.store.remove(key);
            None
        }
    }

    /// 캐시에 값 저장
    pub fn set(&self, key: String, value: V) {
        self.store.insert(
            key,
            CacheEntry {
                value,
                expires_at: Instant::now() + self.default_ttl,
            },
        );
    }

    /// 특정 TTL로 캐시에 값 저장
    pub fn set_with_ttl(&self, key: String, value: V, ttl: Duration) {
        self.store.insert(
            key,
            CacheEntry {
                value,
                expires_at: Instant::now() + ttl,
            },
        );
    }

    /// 캐시 항목 제거
    pub fn remove(&self, key: &str) {
        self.store.remove(key);
    }

    /// 만료된 항목 정리
    pub fn cleanup(&self) {
        let now = Instant::now();
        self.store.retain(|_, v| v.expires_at > now);
    }
}

impl<V: Clone + Send + Sync> CachePort<V> for MemoryCache<V> {
    fn get(&self, key: &str) -> Option<V> {
        self.get(key)
    }

    fn set(&self, key: String, value: V) {
        self.set(key, value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cache_set_get() {
        let cache = MemoryCache::new(Duration::from_secs(60));
        cache.set("key1".to_string(), "value1".to_string());
        assert_eq!(cache.get("key1"), Some("value1".to_string()));
    }

    #[test]
    fn test_cache_miss() {
        let cache: MemoryCache<String> = MemoryCache::new(Duration::from_secs(60));
        assert_eq!(cache.get("nonexistent"), None);
    }

    #[test]
    fn test_cache_expired() {
        let cache = MemoryCache::new(Duration::from_millis(1));
        cache.set("key1".to_string(), "value1".to_string());
        std::thread::sleep(Duration::from_millis(10));
        assert_eq!(cache.get("key1"), None);
    }
}
