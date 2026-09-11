// SPDX-FileCopyrightText: 2026 Alexey Zhokhov
// SPDX-License-Identifier: Apache-2.0

//! Parser-sequence ground-state tracker backing [`DamageGrid::mid_sequence`].
//!
//! vte 0.15 does not expose its parser state, so this tracker observes the
//! same bytes and mirrors vte's state machine just enough to answer "is the
//! parser at ground state": ESC…final, CSI…final, and OSC/DCS/SOS/PM/APC
//! string states terminated by ST (`ESC \`) or BEL (OSC only), with CAN/SUB
//! aborting any sequence, matching vte's `anywhere` transitions. It is
//! deliberately conservative about the parser's internal sub-states: it
//! reports `MidSequence` whenever vte 0.15 is not in `State::Ground`, and
//! `Ground` only then.
//!
//! [`DamageGrid::mid_sequence`]: crate::grid::DamageGrid::mid_sequence

/// Parser sequence state (a coarsening of vte 0.15's `State`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum SeqState {
    /// vte `State::Ground`.
    #[default]
    Ground,
    /// vte `State::Escape` (no intermediates collected yet).
    Escape,
    /// vte `State::EscapeIntermediate`.
    EscapeIntermediate,
    /// vte `State::CsiEntry` / `CsiParam` / `CsiIntermediate` / `CsiIgnore`.
    Csi,
    /// vte `State::OscString` (terminated by BEL or ST).
    Osc,
    /// vte `State::DcsEntry` / `DcsParam` / `DcsIntermediate` /
    /// `DcsPassthrough` / `DcsIgnore` (terminated by ST).
    Dcs,
    /// vte `State::SosPmApcString` (terminated by ST).
    SosPmApc,
}

impl SeqState {
    /// True when the parser is known to be at ground state.
    pub(crate) fn is_ground(self) -> bool {
        self == Self::Ground
    }

    /// Advance the tracker over one byte, mirroring vte 0.15's transitions.
    ///
    /// Fast path: at ground, only `ESC` leaves ground (vte's `advance_ground`
    /// scans for it with memchr; C1 bytes are executed or printed in place).
    pub(crate) fn advance_bytes(&mut self, bytes: &[u8]) {
        let mut rest = bytes;
        while !rest.is_empty() {
            if self.is_ground() {
                match rest.iter().position(|&b| b == 0x1b) {
                    Some(idx) => {
                        self.advance(0x1b);
                        rest = &rest[idx + 1..];
                    }
                    None => return,
                }
            } else {
                self.advance(rest[0]);
                rest = &rest[1..];
            }
        }
    }

    /// vte's `anywhere` transitions: CAN/SUB abort to ground, ESC (re)enters
    /// the escape state, anything else is absorbed.
    fn anywhere(&mut self, byte: u8) {
        match byte {
            0x18 | 0x1a => *self = Self::Ground,
            0x1b => *self = Self::Escape,
            _ => {}
        }
    }

    fn advance(&mut self, byte: u8) {
        match self {
            Self::Ground => {
                if byte == 0x1b {
                    *self = Self::Escape;
                }
            }
            Self::Escape => match byte {
                // C0 controls execute in place.
                0x00..=0x17 | 0x19 | 0x1c..=0x1f => {}
                // Intermediates move to the escape-intermediate state.
                0x20..=0x2f => *self = Self::EscapeIntermediate,
                0x50 => *self = Self::Dcs,                    // ESC P
                0x58 | 0x5e | 0x5f => *self = Self::SosPmApc, // ESC X/^/_
                0x5b => *self = Self::Csi,                    // ESC [
                0x5d => *self = Self::Osc,                    // ESC ]
                // Final bytes dispatch and return to ground.
                0x30..=0x7e => *self = Self::Ground,
                _ => self.anywhere(byte), // 0x7f and high bytes are absorbed
            },
            Self::EscapeIntermediate => match byte {
                0x00..=0x17 | 0x19 | 0x1c..=0x1f => {}
                0x20..=0x2f => {}
                // Any final byte (including `[` or `P`, which only start
                // CSI/DCS directly after ESC) dispatches to ground.
                0x30..=0x7e => *self = Self::Ground,
                _ => self.anywhere(byte),
            },
            Self::Csi => match byte {
                0x00..=0x17 | 0x19 | 0x1c..=0x1f => {}
                // Parameter and intermediate bytes.
                0x20..=0x3f => {}
                // Final bytes.
                0x40..=0x7e => *self = Self::Ground,
                _ => self.anywhere(byte),
            },
            Self::Osc => match byte {
                // BEL terminates OSC.
                0x07 => *self = Self::Ground,
                // ESC begins ST (or aborts into a new escape sequence).
                0x1b => *self = Self::Escape,
                0x18 | 0x1a => *self = Self::Ground,
                _ => {}
            },
            Self::Dcs => match byte {
                0x1b => *self = Self::Escape,
                0x18 | 0x1a => *self = Self::Ground,
                // C1 ST also terminates DCS passthrough in vte.
                0x9c => *self = Self::Ground,
                _ => {}
            },
            Self::SosPmApc => self.anywhere(byte),
        }
    }
}
