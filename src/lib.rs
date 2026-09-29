//! termpane: terminal emulator grid, parser, and damage tracking.
//!
//! **Architecture Invariant:** T0.
//! Entry point: [`DamageGrid`] — terminal grid with damage tracking.

#![deny(missing_docs)]

pub mod cell;
pub mod damage;
pub mod grid;
pub mod passthrough;
#[cfg(all(unix, feature = "process"))]
pub mod process;
#[cfg(all(unix, feature = "pty"))]
pub mod pty;
pub mod snapshot;
pub mod width;

#[cfg(all(not(unix), feature = "process"))]
compile_error!("termpane `process` feature requires a Unix target");
#[cfg(all(not(unix), feature = "pty"))]
compile_error!("termpane `pty` feature requires a Unix target");

pub use cell::{Attrs, Cell, Color, Hyperlink, UnderlineStyle};
pub use damage::{DirtySpans, DirtyTracker};
pub use grid::{DamageGrid, MouseProtocolEncoding, MouseProtocolMode, RowArena, RowWrap, ScrollOp};
pub use passthrough::{PassthroughBuffer, PassthroughEvent};
pub use snapshot::{GridPatch, GridSnapshot, GridView, SnapCell};
pub use width::{Osc8Policy, SupportedSgr, VirtualTerminalProfile, display_width};
