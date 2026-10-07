//! Module-agnostic ops seam (docs/0068 D3): modules own their ops — the
//! bridge core never imports one. A module registers an [`AgentOps`]
//! provider at init (the same place it hands over its settings globals);
//! dispatch resolves ops through the registry at request time.

use std::sync::Arc;

use gpui::{App, Global};

use crate::protocol::RpcError;

/// One module's contribution to the agent bridge.
pub trait AgentOps: Send + Sync {
    /// Ops this provider serves, e.g. `["app_status", "list_repos"]`.
    fn ops(&self) -> &'static [&'static str];
    /// Handle one op on the main thread. Success values MUST carry the
    /// mandatory freshness/truncation envelope (docs/0068 D5) — the
    /// server does not inject it.
    fn handle(
        &self,
        cx: &mut App,
        op: &str,
        params: &serde_json::Value,
    ) -> Result<serde_json::Value, RpcError>;
}

/// Registry of module-registered providers. A gpui global so ops resolve
/// on the main thread, where the module state they read lives (D9).
#[derive(Default)]
pub struct OpsRegistry {
    providers: Vec<Arc<dyn AgentOps>>,
}

impl Global for OpsRegistry {}

impl OpsRegistry {
    pub(crate) fn register(&mut self, provider: Arc<dyn AgentOps>) {
        self.providers.push(provider);
    }

    /// The provider serving `op`, if any. First registration wins; ops
    /// are module-namespaced in practice (`app_status`, …), so collisions
    /// are a programming error — the tests pin the first-wins behavior.
    pub(crate) fn resolve(&self, op: &str) -> Option<Arc<dyn AgentOps>> {
        self.providers
            .iter()
            .find(|provider| provider.ops().contains(&op))
            .cloned()
    }
}

/// Register a provider at module init (the docs/0003 provider seam).
/// Safe to call before or after [`crate::start`] — resolution happens
/// per request.
pub fn register_ops(cx: &mut App, provider: Arc<dyn AgentOps>) {
    cx.default_global::<OpsRegistry>();
    cx.global_mut::<OpsRegistry>().register(provider);
}

#[cfg(test)]
mod tests {
    use super::*;

    struct StubOps {
        ops: &'static [&'static str],
    }

    impl AgentOps for StubOps {
        fn ops(&self) -> &'static [&'static str] {
            self.ops
        }

        fn handle(
            &self,
            _cx: &mut App,
            op: &str,
            _params: &serde_json::Value,
        ) -> Result<serde_json::Value, RpcError> {
            Ok(serde_json::json!({ "echo": op }))
        }
    }

    #[test]
    fn empty_registry_resolves_nothing() {
        let registry = OpsRegistry::default();
        assert!(registry.resolve("app_status").is_none());
    }

    #[test]
    fn registered_provider_serves_its_ops() {
        let mut registry = OpsRegistry::default();
        registry.register(Arc::new(StubOps {
            ops: &["app_status"],
        }));

        let provider = registry.resolve("app_status").unwrap();
        assert_eq!(provider.ops(), &["app_status"][..]);
        // Resolution semantics live in the registry; the handle call is
        // the provider's own (jobs::dispatch invokes it on the main
        // thread — an `App` is not constructible in unit tests).
    }

    #[test]
    fn first_registration_wins_on_op_collision() {
        let mut registry = OpsRegistry::default();
        registry.register(Arc::new(StubOps {
            ops: &["app_status"],
        }));
        registry.register(Arc::new(StubOps {
            // A real collision on `app_status` — the first provider must keep it.
            ops: &["app_status", "list_repos"],
        }));

        let provider = registry.resolve("app_status").unwrap();
        assert_eq!(provider.ops(), &["app_status"][..]);
    }
}
