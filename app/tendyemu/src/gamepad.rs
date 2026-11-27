//! XInput gamepad (DESIGN §6.1; issue #17).
//!
//! macroquad 0.4 dropped its gamepad module and miniquad 0.4 has no gamepad event, so the pad is
//! XInput called through kernel32 by hand: `LoadLibraryExA` + `GetProcAddress`, pinned to
//! System32, `xinput1_4.dll` then `xinput9_1_0.dll`. Resolving at runtime means a machine without
//! XInput reports no pad instead of failing to start, and no crate is added.
//!
//! One pad drives the same logical buttons as the keyboard, and the two merge: the left stick and
//! D-pad add directions, `A B X Y` are the face buttons, the shoulders are `L R`, `Back` is
//! Select and `Start` is Start. A system that lacks a button ignores it downstream (§2.5), so the
//! same pad works for every core.
//!
//! Everything above [`poll`] is pure and testable headless. [`on_frame`] never touches a device
//! until the window loop is running, and never presses anything while the drawer is open.

use std::sync::Mutex;

use tendy_core::Buttons;

use crate::app::App;
use crate::menu::{Row, RowAct};

/// Left-stick deadzone, in stick units (`1.0` is full deflection).
///
/// A resting XInput stick reports a few hundred to a few thousand units of drift. Microsoft's own
/// `XINPUT_GAMEPAD_LEFT_THUMB_DEADZONE` is 7849 / 32767 = 0.24; 0.35 clears a worn stick's drift
/// with room to spare, while a deliberate push registers a third of the way out. The test is per
/// axis and uses `>=`, so the threshold itself already counts: the D-pad is a digital switch.
pub const DEADZONE: f32 = 0.35;

/// The buttons the pad reports: the XInput set. `Guide` is here so a name round-trip can refuse
/// it; nothing binds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum GamepadButton {
    A = 0,
    B = 1,
    X = 2,
    Y = 3,
    LeftShoulder = 4,
    RightShoulder = 5,
    Back = 6,
    Start = 7,
    DpadUp = 8,
    DpadDown = 9,
    DpadLeft = 10,
    DpadRight = 11,
    Guide = 12,
}

impl GamepadButton {
    /// The one-hot bit this button occupies in [`GamepadState::buttons`].
    fn bit(self) -> u16 {
        1 << (self as u8)
    }

    /// The name a binding line uses, e.g. `RightShoulder` — the XInput label spelled out.
    pub fn name(self) -> &'static str {
        match self {
            GamepadButton::A => "A",
            GamepadButton::B => "B",
            GamepadButton::X => "X",
            GamepadButton::Y => "Y",
            GamepadButton::LeftShoulder => "LeftShoulder",
            GamepadButton::RightShoulder => "RightShoulder",
            GamepadButton::Back => "Back",
            GamepadButton::Start => "Start",
            GamepadButton::DpadUp => "DpadUp",
            GamepadButton::DpadDown => "DpadDown",
            GamepadButton::DpadLeft => "DpadLeft",
            GamepadButton::DpadRight => "DpadRight",
            GamepadButton::Guide => "Guide",
        }
    }

    /// Parses a binding name. Case-sensitive, like the key names, so a typo is reported rather
    /// than silently binding something else.
    pub fn from_name(name: &str) -> Option<GamepadButton> {
        Some(match name {
            "A" => GamepadButton::A,
            "B" => GamepadButton::B,
            "X" => GamepadButton::X,
            "Y" => GamepadButton::Y,
            "LeftShoulder" => GamepadButton::LeftShoulder,
            "RightShoulder" => GamepadButton::RightShoulder,
            "Back" => GamepadButton::Back,
            "Start" => GamepadButton::Start,
            "DpadUp" => GamepadButton::DpadUp,
            "DpadDown" => GamepadButton::DpadDown,
            "DpadLeft" => GamepadButton::DpadLeft,
            "DpadRight" => GamepadButton::DpadRight,
            "Guide" => GamepadButton::Guide,
            _ => return None,
        })
    }
}

/// One gamepad's state for one frame, in the app's own terms rather than XInput's.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct GamepadState {
    /// Buttons held this frame, one bit per [`GamepadButton`].
    pub buttons: u16,
    /// Left stick horizontal, `-1.0` (left) to `1.0` (right).
    pub left_x: f32,
    /// Left stick vertical, `-1.0` (down) to `1.0` (up): XInput's own sign for `sThumbLY`.
    pub left_y: f32,
}

/// XInput `wButtons` bit -> the button it means. The stick clicks (`0x0040`, `0x0080`) are left
/// out: no core has such a button and nothing binds them.
const XINPUT_BUTTONS: [(u16, GamepadButton); 12] = [
    (0x0001, GamepadButton::DpadUp),
    (0x0002, GamepadButton::DpadDown),
    (0x0004, GamepadButton::DpadLeft),
    (0x0008, GamepadButton::DpadRight),
    (0x0010, GamepadButton::Start),
    (0x0020, GamepadButton::Back),
    (0x0100, GamepadButton::LeftShoulder),
    (0x0200, GamepadButton::RightShoulder),
    (0x1000, GamepadButton::A),
    (0x2000, GamepadButton::B),
    (0x4000, GamepadButton::X),
    (0x8000, GamepadButton::Y),
];

impl GamepadState {
    /// Builds a state from an XInput gamepad record: the raw `wButtons` word and the two left
    /// stick axes (`-32768..=32767`). Pure, so the conversions are tested without a DLL.
    pub fn from_xinput(buttons: u16, left_x: i16, left_y: i16) -> GamepadState {
        let mut bits = 0_u16;
        for (raw, button) in XINPUT_BUTTONS {
            if buttons & raw != 0 {
                bits |= button.bit();
            }
        }
        GamepadState {
            buttons: bits,
            left_x: axis(left_x),
            left_y: axis(left_y),
        }
    }

    /// Is `button` held this frame?
    pub fn is_down(&self, button: GamepadButton) -> bool {
        self.buttons & button.bit() != 0
    }

    /// The left stick as D-pad bits, after [`DEADZONE`]. Each axis is judged on its own, so a
    /// diagonal push sets both of its directions the way a thumb on a real cross would.
    pub fn stick_dpad(&self) -> Buttons {
        let mut pad = Buttons::NONE;
        if self.left_x >= DEADZONE {
            pad = pad.with(Buttons::RIGHT);
        }
        if self.left_x <= -DEADZONE {
            pad = pad.with(Buttons::LEFT);
        }
        if self.left_y >= DEADZONE {
            pad = pad.with(Buttons::UP);
        }
        if self.left_y <= -DEADZONE {
            pad = pad.with(Buttons::DOWN);
        }
        pad
    }
}

/// One raw stick word as a `-1.0..=1.0` axis. XInput reports `-32768` at full left/down and
/// `32767` at full right/up, so the negative end is clamped.
fn axis(raw: i16) -> f32 {
    (f32::from(raw) / 32_767.0).clamp(-1.0, 1.0)
}

/// The shipped pad layout: face buttons one to one, shoulders `L R`, `Back` Select, `Start`
/// Start, D-pad to D-pad. The left stick adds its directions on top (see [`GamepadState::stick_dpad`]),
/// so a pad with a tired D-pad still steers. No core is asked for a button it lacks: §2.5 drops
/// the extra bits per system.
pub const DEFAULT_BINDINGS: [(GamepadButton, Buttons); 12] = [
    (GamepadButton::A, Buttons::A),
    (GamepadButton::B, Buttons::B),
    (GamepadButton::X, Buttons::X),
    (GamepadButton::Y, Buttons::Y),
    (GamepadButton::LeftShoulder, Buttons::L),
    (GamepadButton::RightShoulder, Buttons::R),
    (GamepadButton::Back, Buttons::SELECT),
    (GamepadButton::Start, Buttons::START),
    (GamepadButton::DpadUp, Buttons::UP),
    (GamepadButton::DpadDown, Buttons::DOWN),
    (GamepadButton::DpadLeft, Buttons::LEFT),
    (GamepadButton::DpadRight, Buttons::RIGHT),
];

/// The pad bits a gamepad contributes: every binding whose button is held, plus the left stick's
/// directions. No pad contributes nothing.
pub fn pad_bits(bindings: &[(GamepadButton, Buttons)], state: Option<&GamepadState>) -> Buttons {
    let Some(state) = state else {
        return Buttons::NONE;
    };
    bindings
        .iter()
        .filter(|(button, _)| state.is_down(*button))
        .fold(state.stick_dpad(), |pad, (_, bit)| pad.with(*bit))
}

/// The keyboard's pad and the gamepad's, as one: both work at once.
pub fn merge(keyboard: Buttons, state: Option<&GamepadState>) -> Buttons {
    pad_bits(&DEFAULT_BINDINGS, state).with(keyboard)
}

/// Polls XInput slot `slot` (`0` is the first pad). `None` means nothing is plugged in or there is
/// no backend on this platform; either way the keyboard runs alone.
pub fn poll(slot: u8) -> Option<GamepadState> {
    backend::poll(slot)
}

/// How many window frames an empty slot is left alone before it is asked again.
///
/// `XInputGetState` on an empty slot makes Windows look for a device — about 20 us a call here,
/// reported as worse with more devices attached. A second of window frames is long enough that
/// the retries disappear from the frame budget, and short enough that a pad plugged in mid-session
/// starts working almost at once.
pub const UNPLUGGED_RETRY_FRAMES: u8 = 60;

/// The per-slot retry countdown: set when a slot answers "no pad", counted down on each frame the
/// slot is skipped. A connected pad is read every frame, so its input is never stale.
#[derive(Debug, Default)]
pub struct PadPoll {
    skip: [u8; 4],
}

impl PadPoll {
    /// Polls `slot` once for one window frame, asking the backend only when the slot is due.
    pub fn poll(&mut self, slot: u8) -> Option<GamepadState> {
        let mut read = || poll(slot);
        self.poll_with(slot, &mut read)
    }

    /// [`poll`](PadPoll::poll) over a caller-supplied read, so the retry rule is testable without
    /// XInput, and on a machine with no backend.
    fn poll_with(
        &mut self,
        slot: u8,
        read: &mut impl FnMut() -> Option<GamepadState>,
    ) -> Option<GamepadState> {
        let skip = self.skip.get_mut(usize::from(slot))?;
        if *skip > 0 {
            *skip -= 1;
            return None;
        }
        let state = read();
        if state.is_none() {
            *skip = UNPLUGGED_RETRY_FRAMES;
        }
        state
    }
}

/// Polls the first pad once per frame, keeping the retry cache beside the window.
static PADS: Mutex<Option<PadPoll>> = Mutex::new(None);

fn poll_first() -> Option<GamepadState> {
    let mut pads = PADS.lock().unwrap_or_else(|poison| poison.into_inner());
    pads.get_or_insert_with(PadPoll::default).poll(0)
}

/// Runs every frame: while a game is running and the window loop is up, polls the pad and hands
/// the machine the keyboard's pad with the pad's bits ORed on. The keys module runs first, so its
/// bits are replaced by the same bits plus the pad's — never lost.
pub fn on_frame(app: &mut App) {
    // Paused, no game, or drawer open: nothing should reach a paused machine, and a pad button
    // held while the drawer closes must not press the game.
    if app.held() {
        return;
    }
    // Headless runs and unit tests never touch a device; the same flag says the window loop —
    // which is also what opens the file dialogs — is running.
    if !crate::file_pick::is_interactive() {
        return;
    }
    let state = poll_first();
    let keyboard = crate::keys::current_pad(app).unwrap_or(Buttons::NONE);
    if let Some(machine) = app.machine_mut() {
        machine.set_buttons(merge(keyboard, state.as_ref()));
    }
}

/// The Input page has no gamepad settings in this slice; the pad's layout is fixed.
pub fn rows(_app: &App) -> Vec<Row> {
    Vec::new()
}

/// Nothing on the Input page is a gamepad row yet.
pub fn on_row(_app: &mut App, _index: usize, _act: RowAct) -> bool {
    false
}

#[cfg(target_os = "windows")]
mod backend {
    //! XInput through kernel32. The load is pinned to the system folder (#449), so a DLL planted
    //! next to the exe is never run.

    use std::ffi::c_void;
    use std::sync::OnceLock;

    use super::GamepadState;

    /// `XINPUT_GAMEPAD`, laid out exactly as the XInput header declares it.
    #[repr(C)]
    #[derive(Default)]
    pub(super) struct XinputGamepad {
        buttons: u16,
        left_trigger: u8,
        right_trigger: u8,
        left_x: i16,
        left_y: i16,
        right_x: i16,
        right_y: i16,
    }

    /// `XINPUT_STATE`: the packet number, then the gamepad record.
    #[repr(C)]
    #[derive(Default)]
    pub(super) struct XinputState {
        packet: u32,
        gamepad: XinputGamepad,
    }

    type GetStateFn = unsafe extern "system" fn(u32, *mut XinputState) -> u32;

    unsafe extern "system" {
        fn LoadLibraryExA(name: *const u8, file: *mut c_void, flags: u32) -> *mut c_void;
        fn GetProcAddress(module: *mut c_void, name: *const u8) -> *mut c_void;
    }

    /// `LOAD_LIBRARY_SEARCH_SYSTEM32`: resolve the name in `%SystemRoot%\System32` only, never in
    /// the exe's own folder (#449).
    const LOAD_LIBRARY_SEARCH_SYSTEM32: u32 = 0x0000_0800;

    /// `xinput1_4.dll` (Windows 8 and newer), then `xinput9_1_0.dll`, which older Windows ships.
    const DLLS: [&[u8]; 2] = [b"xinput1_4.dll\0", b"xinput9_1_0.dll\0"];
    const GET_STATE: &[u8] = b"XInputGetState\0";

    /// Resolves `XInputGetState` once per process. The whole `Option` is cached, so a machine
    /// without XInput pays for one failed `LoadLibraryExA` and nothing after that.
    fn get_state() -> Option<GetStateFn> {
        static RESOLVED: OnceLock<Option<GetStateFn>> = OnceLock::new();
        *RESOLVED.get_or_init(|| {
            for dll in DLLS {
                // SAFETY: the name is the NUL-terminated string above, the file argument is null
                // (the flag names one DLL, not a file to open), and the handle is only used for
                // one GetProcAddress.
                let module = unsafe {
                    LoadLibraryExA(
                        dll.as_ptr(),
                        std::ptr::null_mut(),
                        LOAD_LIBRARY_SEARCH_SYSTEM32,
                    )
                };
                if module.is_null() {
                    continue;
                }
                // SAFETY: the module handle came from LoadLibraryExA and the name is
                // NUL-terminated. A null result just means this DLL does not export the symbol; a
                // non-null one is XInputGetState's address, whose signature is `GetStateFn`.
                let proc = unsafe { GetProcAddress(module, GET_STATE.as_ptr()) };
                if proc.is_null() {
                    continue;
                }
                // SAFETY: see above; the signature is the documented one.
                return Some(unsafe { std::mem::transmute::<*mut c_void, GetStateFn>(proc) });
            }
            None
        })
    }

    /// `0` is `ERROR_SUCCESS`; every other value (notably 1167, `ERROR_DEVICE_NOT_CONNECTED`)
    /// means this slot has no usable pad.
    pub(super) fn poll(slot: u8) -> Option<GamepadState> {
        let get_state = get_state()?;
        if slot > 3 {
            return None;
        }
        let mut state = XinputState::default();
        // SAFETY: `state` is a live, correctly laid out XINPUT_STATE and the slot index is the u32
        // XInput expects; the function writes at most one state and returns a status.
        if unsafe { get_state(u32::from(slot), &mut state) } != 0 {
            return None;
        }
        Some(GamepadState::from_xinput(
            state.gamepad.buttons,
            state.gamepad.left_x,
            state.gamepad.left_y,
        ))
    }
}

#[cfg(not(target_os = "windows"))]
mod backend {
    //! No gamepad backend outside Windows. `poll` is the only platform-specific call, so the
    //! mapping and the merge with the keyboard still work; a build on another OS runs on its keys.
    use super::GamepadState;

    pub(super) fn poll(_slot: u8) -> Option<GamepadState> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(buttons: &[GamepadButton]) -> GamepadState {
        GamepadState {
            buttons: buttons.iter().fold(0, |acc, button| acc | button.bit()),
            ..Default::default()
        }
    }

    fn all_logical() -> Buttons {
        Buttons::ALL
            .iter()
            .fold(Buttons::NONE, |pad, (button, _)| pad.with(*button))
    }

    #[test]
    fn default_bindings_cover_every_pad_button() {
        let mut seen = Buttons::NONE;
        for (_, bit) in DEFAULT_BINDINGS {
            seen = seen.with(bit);
        }
        assert_eq!(seen, all_logical());
    }

    #[test]
    fn face_and_shoulder_buttons_reach_a_and_b() {
        // The face buttons are one to one; the shoulders are L and R.
        for button in [
            GamepadButton::A,
            GamepadButton::B,
            GamepadButton::X,
            GamepadButton::Y,
        ] {
            let expected = match button {
                GamepadButton::A => Buttons::A,
                GamepadButton::B => Buttons::B,
                GamepadButton::X => Buttons::X,
                _ => Buttons::Y,
            };
            assert_eq!(
                pad_bits(&DEFAULT_BINDINGS, Some(&state(&[button]))),
                expected
            );
        }
        assert_eq!(
            pad_bits(
                &DEFAULT_BINDINGS,
                Some(&state(&[GamepadButton::RightShoulder]))
            ),
            Buttons::R
        );
        assert_eq!(
            pad_bits(
                &DEFAULT_BINDINGS,
                Some(&state(&[GamepadButton::LeftShoulder]))
            ),
            Buttons::L
        );
    }

    #[test]
    fn start_back_and_the_dpad_map_one_to_one() {
        let cases = [
            (GamepadButton::Start, Buttons::START),
            (GamepadButton::Back, Buttons::SELECT),
            (GamepadButton::DpadUp, Buttons::UP),
            (GamepadButton::DpadDown, Buttons::DOWN),
            (GamepadButton::DpadLeft, Buttons::LEFT),
            (GamepadButton::DpadRight, Buttons::RIGHT),
        ];
        for (button, bit) in cases {
            assert_eq!(
                pad_bits(&DEFAULT_BINDINGS, Some(&state(&[button]))),
                bit,
                "{button:?}"
            );
        }
    }

    #[test]
    fn no_pad_adds_no_bits() {
        assert_eq!(pad_bits(&DEFAULT_BINDINGS, None), Buttons::NONE);
        assert_eq!(
            pad_bits(&DEFAULT_BINDINGS, Some(&GamepadState::default())),
            Buttons::NONE
        );
    }

    #[test]
    fn the_deadzone_edges_are_exact() {
        let at = |x: f32, y: f32| GamepadState {
            left_x: x,
            left_y: y,
            ..Default::default()
        };
        // Just inside the zone: centred, no direction.
        assert_eq!(at(0.34, 0.34).stick_dpad(), Buttons::NONE);
        assert_eq!(at(-0.34, -0.34).stick_dpad(), Buttons::NONE);
        // Exactly on the threshold: pressed.
        assert_eq!(at(DEADZONE, 0.0).stick_dpad(), Buttons::RIGHT);
        assert_eq!(at(-DEADZONE, 0.0).stick_dpad(), Buttons::LEFT);
        assert_eq!(at(0.0, DEADZONE).stick_dpad(), Buttons::UP);
        assert_eq!(at(0.0, -DEADZONE).stick_dpad(), Buttons::DOWN);
        // Full deflection is full deflection, no overshoot past the bit.
        assert_eq!(at(1.0, 0.0).stick_dpad(), Buttons::RIGHT);
        assert_eq!(at(0.0, 1.0).stick_dpad(), Buttons::UP);
    }

    #[test]
    fn a_stick_diagonal_sets_both_directions() {
        let diagonal = |x: f32, y: f32| GamepadState {
            left_x: x,
            left_y: y,
            ..Default::default()
        };
        let both = |a: Buttons, b: Buttons| a.with(b);
        assert_eq!(
            diagonal(0.9, 0.9).stick_dpad(),
            both(Buttons::RIGHT, Buttons::UP)
        );
        assert_eq!(
            diagonal(-0.9, 0.9).stick_dpad(),
            both(Buttons::LEFT, Buttons::UP)
        );
        assert_eq!(
            diagonal(0.9, -0.9).stick_dpad(),
            both(Buttons::RIGHT, Buttons::DOWN)
        );
        assert_eq!(
            diagonal(-0.9, -0.9).stick_dpad(),
            both(Buttons::LEFT, Buttons::DOWN)
        );
        // Cross-zone: one axis over the threshold, the other under it.
        assert_eq!(diagonal(0.9, 0.2).stick_dpad(), Buttons::RIGHT);
    }

    #[test]
    fn the_stick_and_the_dpad_merge() {
        let mut held = state(&[GamepadButton::DpadRight, GamepadButton::A]);
        held.left_y = 0.8;
        assert_eq!(
            pad_bits(&DEFAULT_BINDINGS, Some(&held)),
            Buttons::RIGHT.with(Buttons::UP).with(Buttons::A)
        );
    }

    #[test]
    fn a_remapped_binding_replaces_the_shipped_one() {
        let bindings = [(GamepadButton::X, Buttons::A)];
        assert_eq!(
            pad_bits(&bindings, Some(&state(&[GamepadButton::X]))),
            Buttons::A
        );
        assert_eq!(
            pad_bits(&bindings, Some(&state(&[GamepadButton::A]))),
            Buttons::NONE
        );
    }

    #[test]
    fn the_xinput_button_word_maps_to_logical_buttons() {
        let cases = [
            (0x0001_u16, GamepadButton::DpadUp),
            (0x0002, GamepadButton::DpadDown),
            (0x0004, GamepadButton::DpadLeft),
            (0x0008, GamepadButton::DpadRight),
            (0x0010, GamepadButton::Start),
            (0x0020, GamepadButton::Back),
            (0x0100, GamepadButton::LeftShoulder),
            (0x0200, GamepadButton::RightShoulder),
            (0x1000, GamepadButton::A),
            (0x2000, GamepadButton::B),
            (0x4000, GamepadButton::X),
            (0x8000, GamepadButton::Y),
        ];
        for (raw, button) in cases {
            let built = GamepadState::from_xinput(raw, 0, 0);
            assert!(built.is_down(button), "{raw:#06x} should be {button:?}");
            // Exactly one button, not a range of them.
            assert_eq!(built.buttons.count_ones(), 1, "{raw:#06x}");
        }
        // A word with several buttons held reports all of them.
        let both = GamepadState::from_xinput(0x1000 | 0x0010, 0, 0);
        assert!(both.is_down(GamepadButton::A) && both.is_down(GamepadButton::Start));
        // The stick clicks are not core buttons: 0x0040 / 0x0080 map to nothing.
        assert_eq!(GamepadState::from_xinput(0x00C0, 0, 0).buttons, 0);
    }

    #[test]
    fn xinput_axes_scale_to_unit_and_clamp() {
        assert_eq!(GamepadState::from_xinput(0, 0, 0).left_x, 0.0);
        assert_eq!(GamepadState::from_xinput(0, 32_767, 0).left_x, 1.0);
        // -32768 is one step past -1.0 and is clamped, so a full-left push still reads LEFT.
        assert_eq!(GamepadState::from_xinput(0, -32_768, 0).left_x, -1.0);
        let mid = GamepadState::from_xinput(0, 0, 16_384);
        assert!((mid.left_y - 0.5).abs() < 0.001, "{}", mid.left_y);
        assert_eq!(mid.stick_dpad(), Buttons::UP);
    }

    #[test]
    fn button_names_round_trip_and_reject_typos() {
        for button in [
            GamepadButton::A,
            GamepadButton::B,
            GamepadButton::X,
            GamepadButton::Y,
            GamepadButton::LeftShoulder,
            GamepadButton::RightShoulder,
            GamepadButton::Back,
            GamepadButton::Start,
            GamepadButton::DpadUp,
            GamepadButton::DpadDown,
            GamepadButton::DpadLeft,
            GamepadButton::DpadRight,
            GamepadButton::Guide,
        ] {
            assert_eq!(GamepadButton::from_name(button.name()), Some(button));
        }
        assert_eq!(GamepadButton::from_name("Dpad"), None);
        assert_eq!(GamepadButton::from_name("a"), None);
    }

    #[test]
    fn a_slot_with_no_pad_polls_as_none() {
        // XInput only has slots 0..=3, so an out-of-range slot must come back as "no pad"; this
        // is the one call that exercises the backend (and the DLL resolution) with no pad.
        assert!(poll(9).is_none());
    }

    #[test]
    fn an_empty_slot_is_asked_about_once_a_second_not_once_a_frame() {
        let mut pad = PadPoll::default();
        let calls = std::cell::Cell::new(0_u32);
        let mut read = || {
            calls.set(calls.get() + 1);
            None
        };
        assert!(pad.poll_with(0, &mut read).is_none());
        assert_eq!(calls.get(), 1, "the first frame asks");
        for frame in 0..UNPLUGGED_RETRY_FRAMES {
            assert!(
                pad.poll_with(0, &mut read).is_none(),
                "skipped frame {frame}"
            );
        }
        assert_eq!(calls.get(), 1, "a second of window frames costs no call");
        assert!(pad.poll_with(0, &mut read).is_none());
        assert_eq!(calls.get(), 2, "the retry asks again");
    }

    #[test]
    fn a_connected_pad_is_read_every_frame() {
        // Nothing about a present pad is cached: every frame reads it, so its buttons and stick
        // are never a frame stale.
        let mut pad = PadPoll::default();
        let calls = std::cell::Cell::new(0_u32);
        let mut read = || {
            calls.set(calls.get() + 1);
            Some(GamepadState::from_xinput(0x1000, 0, 0))
        };
        for frame in 0..10 {
            let state = pad
                .poll_with(0, &mut read)
                .expect("the pad stays connected");
            assert!(state.is_down(GamepadButton::A), "frame {frame}");
        }
        assert_eq!(calls.get(), 10);
    }

    #[test]
    fn a_pad_plugged_in_takes_effect_within_a_second() {
        // The one visible cost of the cache: a pad that appears mid-session is noticed on the
        // next retry, not the next frame.
        let mut pad = PadPoll::default();
        let present = std::cell::Cell::new(false);
        let mut read = || {
            present
                .get()
                .then(|| GamepadState::from_xinput(0x1000, 0, 0))
        };
        assert!(pad.poll_with(1, &mut read).is_none());
        present.set(true);
        let mut frames = 0;
        while pad.poll_with(1, &mut read).is_none() {
            frames += 1;
            assert!(
                frames <= u32::from(UNPLUGGED_RETRY_FRAMES),
                "the pad is never seen"
            );
        }
        assert_eq!(frames, u32::from(UNPLUGGED_RETRY_FRAMES));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn the_xinput_records_have_the_header_layout() {
        // The FFI write is only sound if XINPUT_STATE is 16 bytes with the gamepad record at
        // offset 4, which is what the XInput header says.
        assert_eq!(std::mem::size_of::<backend::XinputGamepad>(), 12);
        assert_eq!(std::mem::size_of::<backend::XinputState>(), 16);
        assert_eq!(std::mem::align_of::<backend::XinputState>(), 4);
    }
}
