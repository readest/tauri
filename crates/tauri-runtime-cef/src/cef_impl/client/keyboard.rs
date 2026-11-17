// Copyright 2019-2024 Tauri Programme within The Commons Conservancy
// SPDX-License-Identifier: Apache-2.0
// SPDX-License-Identifier: MIT

use cef::*;

#[cfg(target_os = "linux")]
type CefOsEvent<'a> = Option<&'a mut cef::sys::XEvent>;
#[cfg(target_os = "macos")]
type CefOsEvent<'a> = *mut u8;
#[cfg(windows)]
type CefOsEvent<'a> = Option<&'a mut cef::sys::MSG>;

wrap_keyboard_handler! {
  pub struct TauriCefKeyboardHandler {
    devtools_enabled: bool,
  }

  impl KeyboardHandler {
    fn on_pre_key_event(
      &self,
      _browser: Option<&mut Browser>,
      event: Option<&KeyEvent>,
      _os_event: CefOsEvent<'_>,
      _is_keyboard_shortcut: Option<&mut ::std::os::raw::c_int>,
    ) -> ::std::os::raw::c_int {
      #[cfg(not(target_os = "macos"))]
      if event.is_some_and(is_reserved_browser_key) {
        if let Some(is_keyboard_shortcut) = _is_keyboard_shortcut {
          *is_keyboard_shortcut = 1;
        }
        return 0;
      }

      // If devtools is disabled, block devtools keyboard shortcuts.
      if !self.devtools_enabled {
        let Some(event) = event else {
          return 0;
        };

        // Check if this is a keydown event.
        use cef::sys::cef_key_event_type_t;
        let keydown_type: cef::KeyEventType = cef_key_event_type_t::KEYEVENT_RAWKEYDOWN.into();
        if event.type_ != keydown_type {
          return 0;
        }

        // Get modifier keys.
        use cef::sys::cef_event_flags_t;
        #[cfg(windows)]
        let modifiers = event.modifiers as i32;
        #[cfg(not(windows))]
        let modifiers = event.modifiers;

        #[cfg(not(target_os = "macos"))]
        let ctrl = (modifiers & (cef_event_flags_t::EVENTFLAG_CONTROL_DOWN.0)) != 0;
        #[cfg(not(target_os = "macos"))]
        let shift = (modifiers & (cef_event_flags_t::EVENTFLAG_SHIFT_DOWN.0)) != 0;

        let key_code = event.windows_key_code;

        // Block F12 (key code 123).
        if key_code == 123 {
          if let Some(is_keyboard_shortcut) = _is_keyboard_shortcut {
            *is_keyboard_shortcut = 1;
          }
          return 1;
        }

        // Block Ctrl+Shift+I (key code 73 = 'I') on Linux/Windows.
        #[cfg(not(target_os = "macos"))]
        if key_code == 73 && ctrl && shift {
          if let Some(is_keyboard_shortcut) = _is_keyboard_shortcut {
            *is_keyboard_shortcut = 1;
          }
          return 1;
        }

        // Block Cmd+Opt+I on macOS.
        #[cfg(target_os = "macos")]
        {
          let meta = (modifiers & cef_event_flags_t::EVENTFLAG_COMMAND_DOWN.0) != 0;
          let alt = (modifiers & cef_event_flags_t::EVENTFLAG_ALT_DOWN.0) != 0;
          if key_code == 73 && meta && alt {
            if let Some(is_keyboard_shortcut) = _is_keyboard_shortcut {
              *is_keyboard_shortcut = 1;
            }
            return 1;
          }
        }
      }

      0
    }
  }
}

/// Whether `event` presses one of the shortcuts Chrome reserves for itself in an
/// app window: Ctrl+W, Ctrl+F4, Ctrl+Shift+W, Alt+F4, Ctrl+T, Ctrl+Shift+T,
/// Ctrl+N or Ctrl+Shift+N.
///
/// Chrome runs a reserved shortcut before the page sees the key
/// (`BrowserCommandController::IsReservedCommandOrKey`), so a page could never bind
/// one. Flagging the key as a shortcut makes CEF answer `NOT_HANDLED_IS_SHORTCUT`,
/// and Chrome skips its early pass. The keydown then reaches the page like any
/// other key, and only one the page leaves alone runs the command, which
/// `command.rs` handles. Chrome reserves Ctrl+PgUp and Ctrl+PgDn too, but with one
/// tab their command is disabled and the key already goes on to the page.
#[cfg(not(target_os = "macos"))]
fn is_reserved_browser_key(event: &KeyEvent) -> bool {
  use cef::sys::{cef_event_flags_t, cef_key_event_type_t};

  let keydown: cef::KeyEventType = cef_key_event_type_t::KEYEVENT_RAWKEYDOWN.into();
  if event.type_ != keydown {
    return false;
  }

  #[cfg(windows)]
  let modifiers = event.modifiers as i32;
  #[cfg(not(windows))]
  let modifiers = event.modifiers;
  let held = |flag: cef_event_flags_t| modifiers & flag.0 != 0;

  is_reserved_chord(
    event.windows_key_code,
    held(cef_event_flags_t::EVENTFLAG_CONTROL_DOWN),
    held(cef_event_flags_t::EVENTFLAG_SHIFT_DOWN),
    held(cef_event_flags_t::EVENTFLAG_ALT_DOWN),
  )
}

/// The key and modifiers half of [`is_reserved_browser_key`].
///
/// Modifiers must match exactly, as they do in Chrome's accelerator table, so
/// AltGr — Ctrl+Alt on Windows — still types its character.
#[cfg(not(target_os = "macos"))]
fn is_reserved_chord(key_code: ::std::os::raw::c_int, ctrl: bool, shift: bool, alt: bool) -> bool {
  const VKEY_N: ::std::os::raw::c_int = 0x4E;
  const VKEY_T: ::std::os::raw::c_int = 0x54;
  const VKEY_W: ::std::os::raw::c_int = 0x57;
  const VKEY_F4: ::std::os::raw::c_int = 0x73;

  match key_code {
    VKEY_W | VKEY_T | VKEY_N => ctrl && !alt,
    VKEY_F4 => !shift && ctrl != alt,
    _ => false,
  }
}

#[cfg(all(test, not(target_os = "macos")))]
mod tests {
  use super::is_reserved_chord;
  use std::os::raw::c_int;

  // Windows virtual key codes, which CEF reports on every platform.
  const F4: c_int = 0x73;
  const I: c_int = 0x49;
  const N: c_int = 0x4E;
  const PAGE_DOWN: c_int = 0x22;
  const Q: c_int = 0x51;
  const T: c_int = 0x54;
  const W: c_int = 0x57;
  const X: c_int = 0x58;

  const CTRL: u8 = 1;
  const SHIFT: u8 = 2;
  const ALT: u8 = 4;

  fn reserved(key_code: c_int, modifiers: u8) -> bool {
    is_reserved_chord(
      key_code,
      modifiers & CTRL != 0,
      modifiers & SHIFT != 0,
      modifiers & ALT != 0,
    )
  }

  #[test]
  fn flags_chromes_reserved_window_and_tab_shortcuts() {
    for (key_code, modifiers) in [
      (W, CTRL),
      (W, CTRL | SHIFT),
      (F4, CTRL),
      (F4, ALT),
      (T, CTRL),
      (T, CTRL | SHIFT),
      (N, CTRL),
      (N, CTRL | SHIFT),
    ] {
      assert!(
        reserved(key_code, modifiers),
        "key {key_code:#x} with modifiers {modifiers:#b} should be flagged"
      );
    }
  }

  #[test]
  fn leaves_other_keys_alone() {
    for (key_code, modifiers) in [
      (W, 0),
      (W, SHIFT),
      // AltGr on Windows arrives as Ctrl+Alt and types a character.
      (W, CTRL | ALT),
      (N, CTRL | ALT),
      (F4, 0),
      (F4, CTRL | SHIFT),
      (F4, CTRL | ALT),
      (PAGE_DOWN, CTRL),
      (Q, CTRL),
      (Q, CTRL | SHIFT),
      (X, CTRL),
      (I, CTRL | SHIFT),
    ] {
      assert!(
        !reserved(key_code, modifiers),
        "key {key_code:#x} with modifiers {modifiers:#b} should not be flagged"
      );
    }
  }
}
