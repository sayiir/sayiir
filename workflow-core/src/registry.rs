//! Task registry for serializable workflows.
//!
//! The registry maps task IDs to their implementations, enabling workflow serialization.
//! Only IDs and structure are serialized; implementations are looked up at runtime.
//!
//! # Registry as Code, Not Data
//!
//! The registry contains closures/functions and cannot be serialized itself.
//! Both the serializing and deserializing sides must build the same registry from code.
//! This is the standard pattern in workflow engines.
//!
//! ```rust,ignore
//! // Shared function - called on both sides (serializer and deserializer)
//! fn build_task_registry(codec: Arc<MyCodec>) -> TaskRegistry {
//!     TaskRegistry::with_codec(codec)
//!         .register("double", |i: u32| async move { Ok(i * 2) })
//!         .register("add_ten", |i: u32| async move { Ok(i + 10) })
//!         .build()
//! }
//!
//! // === Serialization side ===
//! let registry = build_task_registry(codec.clone());
//! let workflow = WorkflowBuilder::new(ctx)
//!     .with_existing_registry(registry)
//!     .then_registered::<u32>("double")
//!     .then_registered::<u32>("add_ten")
//!     .build()?;
//! let serialized = serde_json::to_string(&workflow.to_serializable())?;
//!
//! // === Deserialization side (possibly different process) ===
//! let registry = build_task_registry(codec.clone());  // Rebuild same registry
//! let continuation: SerializableContinuation = serde_json::from_str(&serialized)?;
//! let runnable = continuation.to_runnable(&registry)?;
//! ```

use crate::codec::{Codec, sealed};
use crate::task::{BranchOutputs, CoreTask, UntypedCoreTask, to_heterogeneous_join_task_arc};
use anyhow::Result;
use bytes::Bytes;
use std::collections::HashMap;
use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::Arc;

/// A factory function that creates an UntypedCoreTask.
pub type TaskFactory = Box<dyn Fn() -> UntypedCoreTask + Send + Sync>;

/// Registry for task implementations.
///
/// Maps task IDs to factory functions that create task instances.
/// This enables workflow serialization: only IDs and structure are serialized,
/// and implementations are looked up from the registry at runtime.
///
/// **Important**: The registry is code, not data. It contains closures and cannot
/// be serialized. Both serialization and deserialization sides must construct
/// the same registry by calling the same registration functions. See module docs
/// for the recommended pattern.
pub struct TaskRegistry {
    tasks: HashMap<String, TaskFactory>,
}

impl Default for TaskRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl TaskRegistry {
    /// Create a new empty registry.
    pub fn new() -> Self {
        Self {
            tasks: HashMap::new(),
        }
    }

    /// Register a task using a closure.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// registry.register("double", codec.clone(), |input: u32| async move { Ok(input * 2) });
    /// ```
    pub fn register<I, O, F, Fut, C>(&mut self, id: &str, codec: Arc<C>, func: F)
    where
        F: Fn(I) -> Fut + Send + Sync + 'static,
        I: Send + 'static,
        O: Send + 'static,
        Fut: Future<Output = Result<O>> + Send + 'static,
        C: Codec + sealed::DecodeValue<I> + sealed::EncodeValue<O> + 'static,
    {
        self.register_arc(id, codec, Arc::new(func));
    }

    /// Register a task using an Arc-wrapped closure.
    pub(crate) fn register_arc<I, O, F, Fut, C>(&mut self, id: &str, codec: Arc<C>, func: Arc<F>)
    where
        F: Fn(I) -> Fut + Send + Sync + 'static,
        I: Send + 'static,
        O: Send + 'static,
        Fut: Future<Output = Result<O>> + Send + 'static,
        C: Codec + sealed::DecodeValue<I> + sealed::EncodeValue<O> + 'static,
    {
        let factory = Box::new(move || -> UntypedCoreTask {
            let func = Arc::clone(&func);
            let codec = Arc::clone(&codec);
            Box::new(FnTaskWrapper {
                func,
                codec,
                _phantom: PhantomData,
            })
        });
        self.tasks.insert(id.to_string(), factory);
    }

    /// Register a struct implementing `CoreTask`.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// struct DoubleTask;
    /// impl CoreTask for DoubleTask {
    ///     type Input = u32;
    ///     type Output = u32;
    ///     // ...
    /// }
    ///
    /// registry.register_task("double", codec.clone(), DoubleTask);
    /// ```
    pub fn register_task<T, C>(&mut self, id: &str, codec: Arc<C>, task: T)
    where
        T: CoreTask + Send + Sync + 'static,
        T::Input: Send + 'static,
        T::Output: Send + 'static,
        T::Future: Send + 'static,
        C: Codec + sealed::DecodeValue<T::Input> + sealed::EncodeValue<T::Output> + 'static,
    {
        let task = Arc::new(task);
        let factory = Box::new(move || -> UntypedCoreTask {
            let task = Arc::clone(&task);
            let codec = Arc::clone(&codec);
            Box::new(TaskWrapper { task, codec })
        });
        self.tasks.insert(id.to_string(), factory);
    }

    /// Register a join task using a closure.
    pub fn register_join<O, F, Fut, C>(&mut self, id: &str, codec: Arc<C>, func: F)
    where
        F: Fn(BranchOutputs<C>) -> Fut + Send + Sync + 'static,
        O: Send + 'static,
        Fut: Future<Output = Result<O>> + Send + 'static,
        C: Codec + sealed::EncodeValue<O> + Send + Sync + 'static,
    {
        self.register_arc_join(id, codec, Arc::new(func));
    }

    /// Register a join task using an Arc-wrapped closure.
    pub(crate) fn register_arc_join<O, F, Fut, C>(&mut self, id: &str, codec: Arc<C>, func: Arc<F>)
    where
        F: Fn(BranchOutputs<C>) -> Fut + Send + Sync + 'static,
        O: Send + 'static,
        Fut: Future<Output = Result<O>> + Send + 'static,
        C: Codec + sealed::EncodeValue<O> + Send + Sync + 'static,
    {
        let factory = Box::new(move || -> UntypedCoreTask {
            to_heterogeneous_join_task_arc(Arc::clone(&func), Arc::clone(&codec))
        });
        self.tasks.insert(id.to_string(), factory);
    }

    /// Get a task by ID, creating a new instance.
    ///
    /// Returns `None` if the task ID is not registered.
    pub fn get(&self, id: &str) -> Option<UntypedCoreTask> {
        self.tasks.get(id).map(|factory| factory())
    }

    /// Check if a task ID is registered.
    pub fn contains(&self, id: &str) -> bool {
        self.tasks.contains_key(id)
    }

    /// Get the number of registered tasks.
    pub fn len(&self) -> usize {
        self.tasks.len()
    }

    /// Check if the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }

    /// Get an iterator over registered task IDs.
    pub fn task_ids(&self) -> impl Iterator<Item = &str> {
        self.tasks.keys().map(|s| s.as_str())
    }

    /// Create a builder with a codec for ergonomic task registration.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let registry = TaskRegistry::with_codec(codec)
    ///     .register("double", |i: u32| async move { Ok(i * 2) })
    ///     .register("add_ten", |i: u32| async move { Ok(i + 10) })
    ///     .build();
    /// ```
    pub fn with_codec<C>(codec: Arc<C>) -> RegistryBuilder<C>
    where
        C: Codec,
    {
        RegistryBuilder {
            codec,
            registry: TaskRegistry::new(),
        }
    }
}

/// Builder for ergonomic task registration with a shared codec.
///
/// Created via [`TaskRegistry::with_codec`]. The codec is held internally
/// and used for all registrations, avoiding repetitive `codec.clone()` calls.
pub struct RegistryBuilder<C> {
    codec: Arc<C>,
    registry: TaskRegistry,
}

impl<C: Codec> RegistryBuilder<C> {
    /// Register a task using a closure.
    pub fn register<I, O, F, Fut>(mut self, id: &str, func: F) -> Self
    where
        F: Fn(I) -> Fut + Send + Sync + 'static,
        I: Send + 'static,
        O: Send + 'static,
        Fut: Future<Output = Result<O>> + Send + 'static,
        C: sealed::DecodeValue<I> + sealed::EncodeValue<O> + 'static,
    {
        self.registry.register(id, Arc::clone(&self.codec), func);
        self
    }

    /// Register a struct implementing `CoreTask`.
    pub fn register_task<T>(mut self, id: &str, task: T) -> Self
    where
        T: CoreTask + Send + Sync + 'static,
        T::Input: Send + 'static,
        T::Output: Send + 'static,
        T::Future: Send + 'static,
        C: sealed::DecodeValue<T::Input> + sealed::EncodeValue<T::Output> + 'static,
    {
        self.registry
            .register_task(id, Arc::clone(&self.codec), task);
        self
    }

    /// Register a join task using a closure.
    pub fn register_join<O, F, Fut>(mut self, id: &str, func: F) -> Self
    where
        F: Fn(BranchOutputs<C>) -> Fut + Send + Sync + 'static,
        O: Send + 'static,
        Fut: Future<Output = Result<O>> + Send + 'static,
        C: sealed::EncodeValue<O> + Send + Sync + 'static,
    {
        self.registry
            .register_join(id, Arc::clone(&self.codec), func);
        self
    }

    /// Finish building and return the registry.
    pub fn build(self) -> TaskRegistry {
        self.registry
    }
}

/// Wrapper for closure-based tasks.
struct FnTaskWrapper<F, I, O, C> {
    func: Arc<F>,
    codec: Arc<C>,
    _phantom: PhantomData<fn(I) -> O>,
}

impl<F, I, O, Fut, C> CoreTask for FnTaskWrapper<F, I, O, C>
where
    F: Fn(I) -> Fut + Send + Sync + 'static,
    I: Send + 'static,
    O: Send + 'static,
    Fut: Future<Output = Result<O>> + Send + 'static,
    C: Codec + sealed::DecodeValue<I> + sealed::EncodeValue<O>,
{
    type Input = Bytes;
    type Output = Bytes;
    type Future = Pin<Box<dyn Future<Output = Result<Bytes>> + Send>>;

    fn run(&self, input: Bytes) -> Self::Future {
        let func = Arc::clone(&self.func);
        let codec = Arc::clone(&self.codec);
        Box::pin(async move {
            let decoded_input = codec.decode::<I>(input)?;
            let output = func(decoded_input).await?;
            codec.encode(&output)
        })
    }
}

/// Wrapper for struct-based tasks implementing `CoreTask`.
struct TaskWrapper<T, C> {
    task: Arc<T>,
    codec: Arc<C>,
}

impl<T, C> CoreTask for TaskWrapper<T, C>
where
    T: CoreTask + Send + Sync + 'static,
    T::Input: Send + 'static,
    T::Output: Send + 'static,
    T::Future: Send + 'static,
    C: Codec + sealed::DecodeValue<T::Input> + sealed::EncodeValue<T::Output>,
{
    type Input = Bytes;
    type Output = Bytes;
    type Future = Pin<Box<dyn Future<Output = Result<Bytes>> + Send>>;

    fn run(&self, input: Bytes) -> Self::Future {
        let task = Arc::clone(&self.task);
        let codec = Arc::clone(&self.codec);
        Box::pin(async move {
            let decoded_input = codec.decode::<T::Input>(input)?;
            let output = task.run(decoded_input).await?;
            codec.encode(&output)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{Decoder, Encoder};

    struct DummyCodec;
    impl Encoder for DummyCodec {}
    impl Decoder for DummyCodec {}
    impl sealed::EncodeValue<u32> for DummyCodec {
        fn encode_value(&self, _: &u32) -> Result<Bytes> {
            Ok(Bytes::from_static(b"encoded"))
        }
    }
    impl sealed::DecodeValue<u32> for DummyCodec {
        fn decode_value(&self, _: Bytes) -> Result<u32> {
            Ok(42)
        }
    }

    #[test]
    fn test_registry_register() {
        let mut registry = TaskRegistry::new();
        let codec = Arc::new(DummyCodec);

        registry.register("double", codec, |input: u32| async move { Ok(input * 2) });

        assert!(registry.contains("double"));
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn test_registry_get() {
        let mut registry = TaskRegistry::new();
        let codec = Arc::new(DummyCodec);

        registry.register("double", codec, |input: u32| async move { Ok(input * 2) });

        let task = registry.get("double");
        assert!(task.is_some());

        let missing = registry.get("nonexistent");
        assert!(missing.is_none());
    }

    #[test]
    fn test_registry_task_ids() {
        let mut registry = TaskRegistry::new();
        let codec = Arc::new(DummyCodec);

        registry.register("task_a", codec.clone(), |i: u32| async move { Ok(i) });
        registry.register("task_b", codec.clone(), |i: u32| async move { Ok(i) });
        registry.register("task_c", codec, |i: u32| async move { Ok(i) });

        let mut ids: Vec<_> = registry.task_ids().collect();
        ids.sort();
        assert_eq!(ids, vec!["task_a", "task_b", "task_c"]);
    }
}
