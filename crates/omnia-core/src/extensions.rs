//! Runtime extensions: the type-keyed state slot capability crates install
//! into through the runtime extend hook.

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::sync::{Arc, Mutex, PoisonError};

/// Type-keyed runtime state, shared by the runtime and every store context it
/// builds.
///
/// A capability crate installs its state once — from the deployment's
/// extend hook — and reads it back from the
/// [`Runtime`](crate::Runtime) or, inside a host binding, from the store's
/// [`HasExtensions`](crate::HasExtensions) view. `clone()` is a handle clone:
/// every copy observes the same set.
///
/// An extension value lives inside the runtime's shared state, so one that
/// must call back into the runtime holds a [`WeakRuntime`](crate::WeakRuntime)
/// (via [`Runtime::downgrade`](crate::Runtime::downgrade)); holding a
/// [`Runtime`](crate::Runtime) strongly would leak the runtime through a
/// reference cycle.
#[derive(Clone, Default)]
pub struct Extensions {
    inner: Arc<Mutex<HashMap<TypeId, Arc<dyn Any + Send + Sync>>>>,
}

impl Extensions {
    /// The empty extension set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Install `value` keyed by its type, refusing (returning `false`) when a
    /// value of that type is already installed.
    pub fn insert<T: Send + Sync + 'static>(&self, value: T) -> bool {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        match inner.entry(TypeId::of::<T>()) {
            Entry::Occupied(_) => false,
            Entry::Vacant(slot) => {
                let _ = slot.insert(Arc::new(value));
                true
            }
        }
    }

    /// The installed value of type `T`, if any.
    #[must_use]
    pub fn get<T: Send + Sync + 'static>(&self) -> Option<Arc<T>> {
        let inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let value = Arc::clone(inner.get(&TypeId::of::<T>())?);
        drop(inner);
        // Infallible: the map is keyed by the value's own type.
        value.downcast::<T>().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::Extensions;

    #[test]
    fn insert_once_then_get() {
        let extensions = Extensions::new();
        assert!(extensions.insert(7_u32), "first install succeeds");
        assert!(!extensions.insert(9_u32), "re-install of the same type refuses");
        assert_eq!(*extensions.get::<u32>().expect("installed"), 7);
        assert!(extensions.get::<String>().is_none(), "absent type reads back nothing");
    }

    #[test]
    fn clones_share_set() {
        let extensions = Extensions::new();
        let handle = extensions.clone();
        assert!(extensions.insert("shared".to_owned()));
        assert_eq!(*handle.get::<String>().expect("visible through the clone"), "shared");
    }
}
