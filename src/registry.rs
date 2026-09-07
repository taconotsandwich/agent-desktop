//! Backend registry: ordered per-capability probing.
//!
//! mirrors computer-use-linux's registry shape, adapted to the driver split:
//! each capability (shot / input / windows) probes its candidates in order and
//! the first winner serves that capability. Capabilities win independently —
//! e.g. KWin shot + portal input is a legal combination on a locked-down Plasma.
//!
//! Order per capability (native first, generic X11/EWMH last):
//! - shot: kwin-screenshot2 → portal-screenshot → gnome-shell → x11-maim
//! - input: kwin-eis → portal-remotedesktop → ydotool → xdotool
//! - windows: gnome-ext → gnome-introspect → kwin-script → hyprland → i3 → x11

use crate::drivers::{InputDriver, Probe, ShotDriver, WindowDriver};
use std::sync::Arc;

pub struct Registry {
    pub shot: Arc<dyn ShotDriver>,
    pub input: Arc<dyn InputDriver>,
    pub windows: Arc<dyn WindowDriver>,
    pub probes: Vec<Probe>,
}

impl Registry {
    /// Probe candidates in order; first `ok` wins per capability.
    /// With no native session every probe fails and the caller gets the
    /// ordered failure list — the `doctor` payload.
    pub async fn probe(
        shots: Vec<Arc<dyn ShotDriver>>,
        inputs: Vec<Arc<dyn InputDriver>>,
        windows: Vec<Arc<dyn WindowDriver>>,
    ) -> Result<Self, Vec<Probe>> {
        let mut probes = Vec::with_capacity(shots.len() + inputs.len() + windows.len());
        let mut shot_win: Option<Arc<dyn ShotDriver>> = None;
        for s in shots {
            let p = s.probe().await;
            if p.ok && shot_win.is_none() {
                shot_win = Some(Arc::clone(&s));
            }
            let _ = s;
            probes.push(p);
        }
        let mut input_win: Option<Arc<dyn InputDriver>> = None;
        for i in inputs {
            let p = i.probe().await;
            if p.ok && input_win.is_none() {
                input_win = Some(Arc::clone(&i));
            }
            let _ = i;
            probes.push(p);
        }
        let mut wins_win: Option<Arc<dyn WindowDriver>> = None;
        for w in windows {
            let p = w.probe().await;
            if p.ok && wins_win.is_none() {
                wins_win = Some(Arc::clone(&w));
            }
            let _ = w;
            probes.push(p);
        }
        match (shot_win, input_win, wins_win) {
            (Some(shot), Some(input), Some(windows)) => Ok(Self {
                shot,
                input,
                windows,
                probes,
            }),
            _ => Err(probes),
        }
    }
}
