use crate::platform::drivers::{InputDriver, Probe, ShotDriver, TargetedInputDriver, WindowDriver};
use crate::types::ToolError;
use std::sync::Arc;

#[derive(Clone, Default)]
pub struct Registry {
    pub shot: Option<Arc<dyn ShotDriver>>,
    pub input: Option<Arc<dyn InputDriver>>,
    pub windows: Option<Arc<dyn WindowDriver>>,
    pub targeted: Option<Arc<dyn TargetedInputDriver>>,
    pub probes: Vec<Probe>,
}

impl Registry {
    pub async fn probe(
        shots: Vec<Arc<dyn ShotDriver>>,
        inputs: Vec<Arc<dyn InputDriver>>,
        windows: Vec<Arc<dyn WindowDriver>>,
        targeted: Vec<Arc<dyn TargetedInputDriver>>,
    ) -> Self {
        let mut registry = Self::default();
        for driver in shots {
            let probe = driver.probe().await;
            let ok = probe.ok;
            registry.probes.push(probe);
            if ok {
                registry.shot = Some(driver);
                break;
            }
        }
        for driver in inputs {
            let probe = driver.probe().await;
            let ok = probe.ok;
            registry.probes.push(probe);
            if ok {
                registry.input = Some(driver);
                break;
            }
        }
        for driver in windows {
            let probe = driver.probe().await;
            let ok = probe.ok;
            registry.probes.push(probe);
            if ok {
                registry.windows = Some(driver);
                break;
            }
        }
        for driver in targeted {
            let probe = driver.probe().await;
            let ok = probe.ok;
            registry.probes.push(probe);
            if ok {
                registry.targeted = Some(driver);
                break;
            }
        }
        registry
    }

    pub fn shot(&self) -> Result<&dyn ShotDriver, ToolError> {
        self.shot
            .as_deref()
            .ok_or_else(|| unavailable("screenshots"))
    }
    pub fn input(&self) -> Result<&dyn InputDriver, ToolError> {
        self.input.as_deref().ok_or_else(|| unavailable("input"))
    }
    pub fn windows(&self) -> Result<&dyn WindowDriver, ToolError> {
        self.windows
            .as_deref()
            .ok_or_else(|| unavailable("window targeting"))
    }
    /// Window-addressed input is optional: without it every coordinate
    /// action takes the foreground path.
    pub fn targeted(&self) -> Option<&dyn TargetedInputDriver> {
        self.targeted.as_deref()
    }
}

fn unavailable(capability: &str) -> ToolError {
    ToolError {
        code: "unsupported".into(),
        message: format!(
            "No backend for {capability}; inspect agentdesktop.getState() for probe failures"
        ),
        retryable: false,
    }
}
