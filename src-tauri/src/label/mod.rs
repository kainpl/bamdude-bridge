//! Direct-to-device label printing.
//!
//! The bridge's second role. BamDude renders a label to a 1-bit raster and
//! queues it; this module asks for that work and puts it on a printer hanging
//! off the USB port — something a container on another host cannot do at all.
//!
//! ⚠️ **Nothing here renders anything.** A raster and a copy count arrive over
//! HTTP and are turned into wire packets. The moment this module owns a font it
//! owns a layout, and the bridge stops being a bridge.
//!
//! ## This is a port
//!
//! There is no Niimbot crate — not on crates.io, not as a live project. The
//! reference implementation is `MultiMote/niimbluelib` (TypeScript), and every
//! module below names the file it came from. Only one of seven per-model print
//! flows is ported so far; whoever brings the rest over will be reading the
//! same source, and a port that hides its origin is one they cannot continue.

pub mod encoder;
pub mod packet;
pub mod task;
pub mod transport;
