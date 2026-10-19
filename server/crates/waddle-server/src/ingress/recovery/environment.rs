//! Late-bound execution dependencies, weakly held to avoid an authority/state cycle.

use std::sync::{Arc, Mutex, Weak};

pub trait RecoveryEnvironment: Send + Sync {
    fn recovery_deps(&self) -> crate::server::routes::interpret::Deps<'_>;
}

#[derive(Clone, Default)]
pub(crate) struct RecoveryBinding {
    environment: Arc<Mutex<Option<Weak<dyn RecoveryEnvironment>>>>,
}

impl RecoveryBinding {
    pub(crate) fn bind(&self, environment: Weak<dyn RecoveryEnvironment>) {
        *self
            .environment
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(environment);
    }

    pub(crate) fn environment(&self) -> Option<Arc<dyn RecoveryEnvironment>> {
        self.environment
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .and_then(Weak::upgrade)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Weak};

    use super::{RecoveryBinding, RecoveryEnvironment};
    use crate::server::routes::interpret::Deps;
    use waddle_xmpp::registry::ConnectionRegistry;

    struct Fixture(ConnectionRegistry);

    impl RecoveryEnvironment for Fixture {
        fn recovery_deps(&self) -> Deps<'_> {
            Deps::registry_only(&self.0)
        }
    }

    #[test]
    fn binding_upgrades_only_while_the_environment_lives() {
        let binding = RecoveryBinding::default();
        let coordinator_binding = binding.clone();
        assert!(coordinator_binding.environment().is_none());
        let environment = Arc::new(Fixture(ConnectionRegistry::new()));
        binding.bind(Arc::downgrade(&environment) as Weak<dyn RecoveryEnvironment>);
        assert!(coordinator_binding.environment().is_some());
        drop(environment);
        assert!(coordinator_binding.environment().is_none());
    }
}
