use crate::{
    error::fail,
    types::{Button, ToolError},
};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Request {
    pub target: Option<String>,
    #[serde(flatten)]
    pub operation: Operation,
}

impl Request {
    pub fn target(&self) -> Result<&str, ToolError> {
        self.target
            .as_deref()
            .ok_or_else(|| fail("invalid_argument", "Target is required"))
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "method", content = "args", rename_all = "camelCase")]
pub enum Operation {
    GetState {},
    ListApps {},
    GetApp {
        query: String,
    },
    #[serde(rename = "getAXState")]
    GetAxState(ObservationOptions),
    GetScreenshot {},
    #[serde(rename = "getAXStateAndScreenshot")]
    GetAxStateAndScreenshot(ObservationOptions),
    Click(Click),
    Drag {
        from: PointerTarget,
        to: PointerTarget,
    },
    Scroll(Scroll),
    PressKey {
        key: String,
    },
    TypeText {
        text: String,
    },
    Paste {
        text: String,
        #[serde(default = "text_format")]
        format: String,
    },
    SetValue {
        #[serde(rename = "elementIndex")]
        element_index: u64,
        value: String,
    },
    SelectText(Selection),
    PerformSecondaryAction {
        #[serde(rename = "elementIndex")]
        element_index: u64,
        action: String,
    },
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObservationOptions {
    #[serde(default)]
    pub disable_diffing: bool,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum PointerTarget {
    Element(u64),
    Coordinates([f64; 2]),
}

impl PointerTarget {
    pub fn element(&self) -> Option<u64> {
        match self {
            Self::Element(index) => Some(*index),
            Self::Coordinates(_) => None,
        }
    }

    pub fn coordinates(&self) -> Result<(f64, f64), ToolError> {
        match self {
            Self::Coordinates([x, y]) => Ok((*x, *y)),
            Self::Element(_) => Err(fail("invalid_argument", "Expected [x,y]")),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Click {
    pub target: PointerTarget,
    #[serde(default = "one")]
    pub click_count: u64,
    #[serde(default)]
    pub mouse_button: Button,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Up,
    Down,
    Left,
    Right,
}

impl<'de> Deserialize<'de> for Direction {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        match value.to_ascii_lowercase().as_str() {
            "u" | "up" => Ok(Self::Up),
            "d" | "down" => Ok(Self::Down),
            "l" | "left" => Ok(Self::Left),
            "r" | "right" => Ok(Self::Right),
            other => Err(serde::de::Error::custom(format!(
                "unknown scroll direction {other:?} (use up, down, left, or right)"
            ))),
        }
    }
}

impl Direction {
    pub fn horizontal(self) -> bool {
        matches!(self, Self::Left | Self::Right)
    }
    pub fn delta(self, amount: f64) -> (f64, f64) {
        match self {
            Self::Up => (0.0, -amount),
            Self::Down => (0.0, amount),
            Self::Left => (-amount, 0.0),
            Self::Right => (amount, 0.0),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct Scroll {
    pub target: PointerTarget,
    pub direction: Direction,
    #[serde(default = "one_page")]
    pub pages: f64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Selection {
    pub element_index: u64,
    pub text: String,
    #[serde(default)]
    pub prefix: String,
    #[serde(default)]
    pub suffix: String,
    #[serde(default = "text_format")]
    pub selection_type: String,
}

fn one() -> u64 {
    1
}
fn one_page() -> f64 {
    1.0
}
fn text_format() -> String {
    "text".into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn scroll_directions_parse_case_insensitively() {
        for (value, expected) in [
            ("Down", Direction::Down),
            ("UP", Direction::Up),
            ("l", Direction::Left),
            ("right", Direction::Right),
        ] {
            assert_eq!(
                serde_json::from_value::<Direction>(json!(value)).unwrap(),
                expected
            );
        }
        let error = serde_json::from_value::<Direction>(json!("sideways")).unwrap_err();
        assert!(error.to_string().contains("unknown scroll direction"));
    }
}
