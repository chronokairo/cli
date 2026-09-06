//! Native Zero-Lib Terminal UI Engine based on microsoft/edit architecture.

pub mod framebuffer;
pub mod vt;

#[allow(unused_imports)]
pub use framebuffer::{Cell, Framebuffer};
#[allow(unused_imports)]
pub use vt::{Color, Line, Span, Style, Vt};

