pub mod accessibility;
mod actions;
pub mod apps;
mod clipboard;
pub mod geometry;
#[cfg(test)]
mod tests;

use crate::{
    desktop::accessibility::{self as a11y, AtspiConnection, FlatElement},
    desktop::apps::Application,
    error::fail,
    platform::drivers::{SessionType, ShotTarget, WindowInfo},
    platform::registry::Registry,
    request::{Operation, PointerTarget, Request},
    types::ToolError,
};
use base64::Engine as _;
use geometry::Frame;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::Mutex;

#[derive(Default)]
struct Observation {
    elements: HashMap<u64, FlatElement>,
    previous: HashMap<String, String>,
    frame: Option<Frame>,
    window: Option<String>,
    observed_at: Option<Instant>,
    observed_geometry: Option<crate::platform::drivers::Bbox>,
}

#[derive(Default)]
struct State {
    apps: HashMap<String, Application>,
    observations: HashMap<String, Observation>,
    next_index: u64,
}

pub struct Engine {
    pub registry: Registry,
    pub session: SessionType,
    pub atspi: Arc<AtspiConnection>,
    state: Mutex<State>,
    pub mutation: Mutex<()>,
}

impl Engine {
    pub fn new(registry: Registry, session: SessionType, atspi: Arc<AtspiConnection>) -> Self {
        Self {
            registry,
            session,
            atspi,
            state: Mutex::new(State::default()),
            mutation: Mutex::new(()),
        }
    }

    pub async fn reset(&self) {
        *self.state.lock().await = State::default();
    }

    pub async fn dispatch(&self, request: Request) -> Result<Value, ToolError> {
        match &request.operation {
            Operation::GetState {} => self.desktop_state().await,
            Operation::ListApps {} => Ok(json!(apps::catalog())),
            Operation::GetApp { query } => self.get_app(query).await,
            Operation::GetAxState(options) => {
                self.ax_state(request.target()?, options.disable_diffing)
                    .await
            }
            Operation::GetScreenshot {} => self.screenshot(request.target()?).await,
            Operation::GetAxStateAndScreenshot(options) => {
                let id = request.target()?;
                let state = self.ax_state(id, options.disable_diffing).await?;
                let screenshot = self.screenshot(id).await?;
                Ok(json!({"state":state,"screenshot":screenshot}))
            }
            _ => self.perform(request).await,
        }
    }

    async fn desktop_state(&self) -> Result<Value, ToolError> {
        let windows = match self.registry.windows() {
            Ok(driver) => driver.query().await?,
            Err(_) => vec![],
        };
        let (a11y_ok, a11y_detail) = a11y::status(&self.atspi).await;
        Ok(json!({
            "applications": apps::catalog(), "windows": windows,
            "capabilities": {
                "screenshot": self.registry.shot.as_ref().map(|driver|driver.id()),
                "windowScreenshot": self.registry.shot.as_ref().is_some_and(|driver|driver.window_capture()),
                "input": self.registry.input.as_ref().map(|driver|driver.id()),
                "windows": self.registry.windows.as_ref().map(|driver|driver.id()),
                "accessibility": {"available":a11y_ok,"detail":a11y_detail},
                "coordinateInput":"foreground", "semanticInput":"application-dependent"
            },
            "probes":self.registry.probes, "session":self.session
        }))
    }

    async fn get_app(&self, query: &str) -> Result<Value, ToolError> {
        let app = apps::find(query)?;
        let windows = self.registry.windows()?.query().await?;
        if !windows.iter().any(|window| app.owns(window)) {
            app.launch().await?;
            let deadline = Instant::now() + Duration::from_secs(25);
            loop {
                if self
                    .registry
                    .windows()?
                    .query()
                    .await?
                    .iter()
                    .any(|window| app.owns(window))
                {
                    break;
                }
                if Instant::now() >= deadline {
                    return Err(fail(
                        "launch_timeout",
                        "Application did not expose a window",
                    ));
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
        let id = app.id.clone();
        self.state.lock().await.apps.insert(id.clone(), app);
        // Compositor window lists can flicker between the selection probe and
        // the first observation (splash teardown, extension refresh). Ride out
        // a transient empty list instead of reporting the app as closed.
        let deadline = Instant::now() + Duration::from_secs(10);
        let state = loop {
            match self.ax_state(&id, true).await {
                Ok(state) => break state,
                Err(error) if error.code == "app_closed" && Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                Err(error) => return Err(error),
            }
        };
        Ok(json!({"id":id,"state":state}))
    }

    async fn window(&self, id: &str) -> Result<WindowInfo, ToolError> {
        let state = self.state.lock().await;
        let app = state
            .apps
            .get(id)
            .ok_or_else(|| {
                fail(
                    "stale_target",
                    "Select the application with agentdesktop.getApp()",
                )
            })?
            .clone();
        let last = state
            .observations
            .get(id)
            .and_then(|observation| observation.window.clone());
        drop(state);
        let windows: Vec<_> = self
            .registry
            .windows()?
            .query()
            .await?
            .into_iter()
            .filter(|window| app.owns(window))
            .collect();
        if let Some(window) = windows.iter().find(|window| window.is_active) {
            return Ok(window.clone());
        }
        if let Some(window) = windows
            .iter()
            .find(|window| Some(&window.window_ref.0) == last.as_ref())
        {
            return Ok(window.clone());
        }
        if windows.len() == 1 {
            return Ok(windows[0].clone());
        }
        Err(fail(
            if windows.is_empty() {
                "app_closed"
            } else {
                "ambiguous_window"
            },
            "Application has no unambiguous current window; inspect agentdesktop.getState()",
        ))
    }

    async fn ax_state(&self, id: &str, full: bool) -> Result<Value, ToolError> {
        let mut attempts = 0;
        let (window, observed) = loop {
            let window = self.window(id).await?;
            let observed = tokio::time::timeout(
                Duration::from_secs(20),
                a11y::target::elements(&self.atspi, &window),
            )
            .await;
            let current = self.window(id).await?;
            if current.window_ref == window.window_ref && current.geometry == window.geometry {
                break (window, observed);
            }
            attempts += 1;
            if attempts >= 3 {
                return Err(fail(
                    "unstable_window",
                    "Application is still changing windows; observe again",
                ));
            }
        };
        let (elements, truncated, warnings) = match observed {
            Ok(Ok(result)) => result,
            Ok(Err(error)) => (
                vec![],
                false,
                vec![format!("{}: {}", error.code, error.message)],
            ),
            Err(_) => (
                vec![],
                true,
                vec!["Accessibility observation timed out".into()],
            ),
        };
        let mut state = self.state.lock().await;
        let old = state.observations.remove(id).unwrap_or_default();
        let mut observation = Observation {
            window: Some(window.window_ref.0.clone()),
            frame: old.frame.filter(|frame| {
                old.window.as_ref() == Some(&window.window_ref.0)
                    && frame.geometry == window.geometry
            }),
            observed_at: Some(Instant::now()),
            observed_geometry: Some(window.geometry.clone()),
            ..Observation::default()
        };
        let mut rows = Vec::new();
        let mut changed = Vec::new();
        for element in elements {
            state.next_index += 1;
            let index = state.next_index;
            let mut row = serde_json::to_value(&element)
                .map_err(|error| fail("internal", error.to_string()))?;
            let signature = row.to_string();
            if old.previous.get(&element.id.0) != Some(&signature) {
                changed.push(index);
            }
            observation.previous.insert(element.id.0.clone(), signature);
            row["index"] = json!(index);
            rows.push(row);
            observation.elements.insert(index, element);
        }
        let removed: Vec<_> = old
            .previous
            .keys()
            .filter(|reference| !observation.previous.contains_key(*reference))
            .cloned()
            .collect();
        state.observations.insert(id.into(), observation);
        Ok(
            json!({"app":id,"window":window,"elements":rows,"truncated":truncated,"warnings":warnings,
            "diff": if full || old.previous.is_empty() {Value::Null} else {json!({"changed":changed,"removed":removed})}}),
        )
    }

    async fn screenshot(&self, id: &str) -> Result<Value, ToolError> {
        for attempt in 0..3 {
            match self.capture_frame(id).await {
                Err(error) if error.code == "stale_screenshot" && attempt < 2 => {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                result => return result,
            }
        }
        unreachable!("last capture attempt returns its result")
    }

    async fn capture_frame(&self, id: &str) -> Result<Value, ToolError> {
        let window = self.window(id).await?;
        let geometry = window.geometry.clone();
        let shot = self
            .registry
            .shot()?
            .capture(
                ShotTarget::Window {
                    id: window.window_ref.clone(),
                    geometry: geometry.clone(),
                },
                1280,
            )
            .await?;
        let current = self.window(id).await?;
        if current.window_ref != window.window_ref || current.geometry != geometry {
            return Err(fail(
                "stale_screenshot",
                "Window changed during capture; take a fresh screenshot",
            ));
        }
        let frame = Frame::from_shot(&shot, geometry)?;
        let mut state = self.state.lock().await;
        let observation = state.observations.entry(id.into()).or_default();
        if observation.window.as_ref() != Some(&window.window_ref.0) {
            observation.elements.clear();
            observation.observed_at = None;
        }
        observation.window = Some(window.window_ref.0);
        observation.frame = Some(frame.clone());
        Ok(
            json!({"data":base64::engine::general_purpose::STANDARD.encode(shot.bytes),"mimeType":"image/png","frame":frame}),
        )
    }

    async fn element(&self, id: &str, index: u64) -> Result<FlatElement, ToolError> {
        let window = self.window(id).await?;
        let state = self.state.lock().await;
        let observation = state
            .observations
            .get(id)
            .ok_or_else(|| fail("stale_element", "Observe the application first"))?;
        if observation
            .observed_at
            .is_none_or(|time| time.elapsed() > Duration::from_secs(60))
            || observation.window.as_ref() != Some(&window.window_ref.0)
            || observation.observed_geometry.as_ref() != Some(&window.geometry)
        {
            return Err(fail(
                "stale_element",
                "Observation expired or window bounds changed; get fresh state",
            ));
        }
        observation.elements.get(&index).cloned().ok_or_else(|| {
            fail(
                "stale_element",
                "Index is not from this target's latest observation",
            )
        })
    }

    async fn point(&self, id: &str, value: &PointerTarget) -> Result<(i32, i32), ToolError> {
        let window = self.window(id).await?;
        let (x, y) = value.coordinates()?;
        let state = self.state.lock().await;
        let observation = state
            .observations
            .get(id)
            .filter(|observation| observation.window.as_ref() == Some(&window.window_ref.0));
        let frame = observation
            .and_then(|observation| observation.frame.as_ref())
            .ok_or_else(|| {
                fail(
                    "stale_screenshot",
                    "Take a target screenshot before coordinate actions",
                )
            })?;
        if frame.geometry != window.geometry {
            return Err(fail(
                "stale_screenshot",
                "Window geometry changed; take a fresh screenshot",
            ));
        }
        frame.point(x, y)
    }

    async fn focus(&self, id: &str) -> Result<(), ToolError> {
        self.registry.input()?.prepare().await?;
        let window = self.window(id).await?;
        if !window.is_active {
            self.registry.windows()?.focus(&window.window_ref).await?;
            let deadline = Instant::now() + Duration::from_secs(2);
            while !self.window(id).await?.is_active {
                if Instant::now() >= deadline {
                    return Err(fail(
                        "focus_failed",
                        "Target did not become active; input was not sent",
                    ));
                }
                tokio::time::sleep(Duration::from_millis(30)).await;
            }
        }
        Ok(())
    }

    async fn position_pointer(&self, id: &str, x: i32, y: i32) -> Result<(), ToolError> {
        self.registry.input()?.move_to(x, y).await?;
        // Moving out of a hot corner can open a desktop overview. Restore
        // target focus after motion, before dispatching buttons or scrolling.
        self.focus(id).await
    }

    async fn invalidate(&self, id: &str) {
        if let Some(observation) = self.state.lock().await.observations.get_mut(id) {
            observation.elements.clear();
        }
    }
}
