use crate::event::RaptorEvent;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

/// 属性值类型
#[derive(Debug, Clone, PartialEq)]
pub enum PropertyValue {
    String(String),
    Int(i64),
    Float(f64),
    Bool(bool),
}

impl PropertyValue {
    /// 序列化为 JSON 字符串
    pub fn to_json(&self) -> String {
        match self {
            PropertyValue::String(s) => format!("\"{}\"", s.replace('"', "\\\"")),
            PropertyValue::Int(n) => n.to_string(),
            PropertyValue::Float(f) => f.to_string(),
            PropertyValue::Bool(b) => b.to_string(),
        }
    }
}

/// 属性观察者回调
pub type PropertyObserver = Arc<dyn Fn(&PropertyValue) + Send + Sync>;

/// 观察者 ID 计数器
static OBSERVER_ID_COUNTER: AtomicI64 = AtomicI64::new(1);

fn next_observer_id() -> i64 {
    OBSERVER_ID_COUNTER.fetch_add(1, Ordering::Relaxed)
}

/// 属性存储 trait
pub trait PropertyStore: Send + Sync {
    fn get(&self, key: &str) -> Option<PropertyValue>;
    fn set(&self, key: &str, value: PropertyValue);
    fn observe(&self, key: &str, callback: PropertyObserver) -> i64;
    fn unobserve(&self, key: &str, observer_id: i64) -> bool;
}

/// 默认属性存储实现 — 支持观察者模式
pub struct DefaultPropertyStore {
    store: RwLock<HashMap<String, PropertyValue>>,
    observers: RwLock<HashMap<String, Vec<(i64, PropertyObserver)>>>,
    #[allow(dead_code)]
    event_tx: tokio::sync::mpsc::UnboundedSender<RaptorEvent>,
}

impl DefaultPropertyStore {
    pub fn new(event_tx: tokio::sync::mpsc::UnboundedSender<RaptorEvent>) -> Self {
        Self {
            store: RwLock::new(HashMap::new()),
            observers: RwLock::new(HashMap::new()),
            event_tx,
        }
    }
}

impl PropertyStore for DefaultPropertyStore {
    fn get(&self, key: &str) -> Option<PropertyValue> {
        self.store.read().get(key).cloned()
    }

    fn set(&self, key: &str, value: PropertyValue) {
        self.store.write().insert(key.to_string(), value.clone());

        // 通知观察者
        let observers = self.observers.read();
        if let Some(observer_list) = observers.get(key) {
            for (_id, callback) in observer_list {
                callback(&value);
            }
        }
    }

    fn observe(&self, key: &str, callback: PropertyObserver) -> i64 {
        let id = next_observer_id();
        self.observers
            .write()
            .entry(key.to_string())
            .or_default()
            .push((id, callback));
        id
    }

    fn unobserve(&self, key: &str, observer_id: i64) -> bool {
        let mut observers = self.observers.write();
        if let Some(list) = observers.get_mut(key) {
            let before = list.len();
            list.retain(|(id, _)| *id != observer_id);
            list.len() < before
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_store() -> DefaultPropertyStore {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        DefaultPropertyStore::new(tx)
    }

    #[test]
    fn test_get_set() {
        let store = make_store();
        store.set("volume", PropertyValue::Int(80));
        assert_eq!(store.get("volume"), Some(PropertyValue::Int(80)));
    }

    #[test]
    fn test_get_missing() {
        let store = make_store();
        assert_eq!(store.get("nonexistent"), None);
    }

    #[test]
    fn test_set_different_types() {
        let store = make_store();
        store.set("name", PropertyValue::String("test".into()));
        store.set("volume", PropertyValue::Int(50));
        store.set("position", PropertyValue::Float(1.5));
        store.set("muted", PropertyValue::Bool(false));

        assert_eq!(
            store.get("name"),
            Some(PropertyValue::String("test".into()))
        );
        assert_eq!(store.get("volume"), Some(PropertyValue::Int(50)));
        assert_eq!(store.get("position"), Some(PropertyValue::Float(1.5)));
        assert_eq!(store.get("muted"), Some(PropertyValue::Bool(false)));
    }

    #[test]
    fn test_observer_triggered() {
        let store = make_store();
        let triggered = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let triggered_clone = triggered.clone();

        store.observe(
            "volume",
            Arc::new(move |_val| {
                triggered_clone.store(true, Ordering::Relaxed);
            }),
        );

        store.set("volume", PropertyValue::Int(90));
        assert!(triggered.load(Ordering::Relaxed));
    }

    #[test]
    fn test_observer_receives_value() {
        let store = make_store();
        let received = Arc::new(parking_lot::Mutex::new(None));
        let received_clone = received.clone();

        store.observe(
            "volume",
            Arc::new(move |val| {
                *received_clone.lock() = Some(val.clone());
            }),
        );

        store.set("volume", PropertyValue::Int(42));
        assert_eq!(*received.lock(), Some(PropertyValue::Int(42)));
    }

    #[test]
    fn test_unobserve() {
        let store = make_store();
        let triggered = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let triggered_clone = triggered.clone();

        let id = store.observe(
            "volume",
            Arc::new(move |_val| {
                triggered_clone.store(true, Ordering::Relaxed);
            }),
        );

        assert!(store.unobserve("volume", id));
        store.set("volume", PropertyValue::Int(100));
        assert!(!triggered.load(Ordering::Relaxed));
    }

    #[test]
    fn test_unobserve_nonexistent() {
        let store = make_store();
        assert!(!store.unobserve("volume", 999));
    }

    #[test]
    fn test_multiple_observers() {
        let store = make_store();
        let count = Arc::new(std::sync::atomic::AtomicU32::new(0));

        for _ in 0..3 {
            let count_clone = count.clone();
            store.observe(
                "volume",
                Arc::new(move |_val| {
                    count_clone.fetch_add(1, Ordering::Relaxed);
                }),
            );
        }

        store.set("volume", PropertyValue::Int(50));
        assert_eq!(count.load(Ordering::Relaxed), 3);
    }
}
