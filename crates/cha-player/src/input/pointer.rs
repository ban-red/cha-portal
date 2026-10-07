//! Mouse buttons, wheel and absolute position, in the browser player's model.

use winit::event::{MouseButton, MouseScrollDelta};

use crate::render::video::Viewport;

/// `PointerEvent.button`: 0 left, 1 middle, 2 right, 3 back, 4 forward.
pub fn button(button: MouseButton) -> Option<u8> {
    match button {
        MouseButton::Left => Some(0),
        MouseButton::Middle => Some(1),
        MouseButton::Right => Some(2),
        MouseButton::Back => Some(3),
        MouseButton::Forward => Some(4),
        MouseButton::Other(_) => None,
    }
}

/// Pixels a browser reports for one wheel notch.
const NOTCH: f32 = 100.0;

/// A wheel event as a browser reports it: pixels, 100 per notch, y down.
/// winit's deltas point the other way (positive scrolls the content up/right
/// away from the finger), so both axes flip.
pub fn wheel(delta: MouseScrollDelta) -> (f32, f32) {
    match delta {
        MouseScrollDelta::LineDelta(x, y) => (-x * NOTCH, -y * NOTCH),
        MouseScrollDelta::PixelDelta(p) => (-p.x as f32, -p.y as f32),
    }
}

/// A window position (physical pixels) as 0..1 of the picture, or `None`
/// outside it.
pub fn absolute(position: (f64, f64), picture: Viewport) -> Option<(f32, f32)> {
    if picture.width < 1.0 || picture.height < 1.0 {
        return None;
    }
    let x = (position.0 as f32 - picture.x) / picture.width;
    let y = (position.1 as f32 - picture.y) / picture.height;
    ((0.0..=1.0).contains(&x) && (0.0..=1.0).contains(&y)).then_some((x, y))
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::dpi::PhysicalPosition;

    #[test]
    fn a_notch_is_100_pixels_down() {
        // winit: negative y = the wheel turned toward the user = scroll down.
        assert_eq!(wheel(MouseScrollDelta::LineDelta(0.0, -1.0)), (0.0, 100.0));
        assert_eq!(wheel(MouseScrollDelta::LineDelta(1.0, 0.0)), (-100.0, 0.0));
    }

    #[test]
    fn trackpad_pixels_pass_through_flipped() {
        let d = MouseScrollDelta::PixelDelta(PhysicalPosition::new(3.0, -12.0));
        assert_eq!(wheel(d), (-3.0, 12.0));
    }

    #[test]
    fn buttons_follow_pointer_event() {
        assert_eq!(button(MouseButton::Left), Some(0));
        assert_eq!(button(MouseButton::Right), Some(2));
        assert_eq!(button(MouseButton::Forward), Some(4));
        assert_eq!(button(MouseButton::Other(9)), None);
    }

    #[test]
    fn absolute_position_is_relative_to_the_picture() {
        let picture = Viewport {
            x: 100.0,
            y: 0.0,
            width: 800.0,
            height: 600.0,
        };
        assert_eq!(absolute((500.0, 300.0), picture), Some((0.5, 0.5)));
        assert_eq!(absolute((50.0, 300.0), picture), None, "in the letterbox");
    }
}
