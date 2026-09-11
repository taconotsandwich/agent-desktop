use crate::{
    error::fail,
    platform::drivers::{Bbox, Shot},
    types::ToolError,
};
use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
pub struct Frame {
    pub geometry: Bbox,
    pub image_width: u32,
    pub image_height: u32,
}

impl Frame {
    pub fn from_shot(shot: &Shot, geometry: Bbox) -> Result<Self, ToolError> {
        let image = image::load_from_memory(&shot.bytes)
            .map_err(|error| fail("invalid_screenshot", error.to_string()))?;
        if geometry.w == 0 || geometry.h == 0 {
            return Err(fail("invalid_screenshot", "Empty screenshot geometry"));
        }
        Ok(Self {
            geometry,
            image_width: image.width(),
            image_height: image.height(),
        })
    }

    pub fn point(&self, x: f64, y: f64) -> Result<(i32, i32), ToolError> {
        if !x.is_finite()
            || !y.is_finite()
            || x < 0.0
            || y < 0.0
            || x >= self.image_width as f64
            || y >= self.image_height as f64
        {
            return Err(fail(
                "invalid_coordinate",
                "Coordinates must be inside the latest target screenshot",
            ));
        }
        Ok((
            self.geometry.x + (x * self.geometry.w as f64 / self.image_width as f64).floor() as i32,
            self.geometry.y
                + (y * self.geometry.h as f64 / self.image_height as f64).floor() as i32,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn maps_scaled_crop_with_negative_monitor_origin() {
        let frame = Frame {
            geometry: Bbox {
                x: -1920,
                y: 100,
                w: 1600,
                h: 900,
            },
            image_width: 800,
            image_height: 450,
        };
        assert_eq!(frame.point(400.0, 225.0).unwrap(), (-1120, 550));
        assert!(frame.point(800.0, 0.0).is_err());
        assert!(frame.point(f64::NAN, 0.0).is_err());
    }
}
