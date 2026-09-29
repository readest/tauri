// Copyright 2019-2024 Tauri Programme within The Commons Conservancy
// SPDX-License-Identifier: Apache-2.0
// SPDX-License-Identifier: MIT

//! Chromium preferences and content settings, applied to every webview's request context
//! and to the browser-wide preference store.
//!
//! The CEF runtime creates Chrome style browsers, so each webview is backed by a real
//! Chrome profile and inherits the browser-shaped behaviour that comes with it: a "Save
//! password?" bubble on any form submit, address and credit-card save bubbles, a
//! "Translate this page?" bubble, and a handful of background requests to Google. None of
//! that belongs in an application webview, so we turn it off per request context right
//! after the profile finishes initializing.
//!
//! # Two stores, two lifetimes
//!
//! A *profile* preference lives on the request context and can only be written once that
//! context's underlying Chromium `Profile` exists — which is what
//! `on_request_context_initialized` signals. A *global* preference lives in Chromium's
//! local state and is reachable through `cef::preference_manager_get_global()` once the
//! CEF context is initialized. `devtools.availability` is the first kind;
//! `devtools.remote_debugging.allowed` is the second.
//!
//! # Safe Browsing stays on
//!
//! [`PREFERENCES`] deliberately leaves `safebrowsing.enabled` alone. A Tauri webview
//! routinely loads content the developer does not control - OAuth and SSO flows, embedded
//! third-party pages, iframes, and the popups this runtime supports - so it is not the
//! closed world that would make the protection pointless, and standard protection is a
//! local hash-prefix database rather than a per-navigation callback to Google.
//!
//! An application whose webview only ever loads its own content can still opt out with
//! `Cef::safe_browsing(false)`, which is also how any entry in [`PREFERENCES`] is turned
//! back on.

use cef::{
  CefString, ContentSettingTypes, ContentSettingValues, ImplDictionaryValue, ImplListValue,
  ImplPreferenceManager, ImplRequestContext, ImplValue, RequestContext, Value,
};

/// Builds the `error` out-parameter that every
/// [`ImplPreferenceManager::set_preference`] call has to pass.
///
/// `CefPreferenceManager::SetPreference` marks only `value` as optional, so CEF's
/// shim opens with `DCHECK(error); if (!error) { return 0; }` and a [`None`] error
/// makes the call fail before the preference service is ever consulted.
///
/// [`CefString::default`] is not a substitute: it builds the borrowed-none
/// variant, which converts to a null pointer again. `CefString::from("")` builds
/// the owned variant, which converts to a real, writable pointer and frees
/// whatever CEF stores in it when the string is dropped.
pub(crate) fn set_preference_error_slot() -> CefString {
  CefString::from("")
}

/// `devtools.availability`, the profile preference `DevToolsWindow::AllowDevToolsFor`
/// consults on every path that opens DevTools.
///
/// CEF gates more than the DevTools window on it: `CefBrowserHost::SendDevToolsMessage`
/// is refused on a profile where this is `kDisallowed`, and refused silently — the call
/// still reports success and no response, result or event is ever delivered to a
/// `CefDevToolsMessageObserver`. That makes it unusable as a hardening switch here, since
/// the runtime drives its own startup over the DevTools protocol. See
/// [`DevToolsPolicy`](crate::DevToolsPolicy).
pub(crate) const DEVTOOLS_AVAILABILITY: &str = "devtools.availability";

/// The default `devtools.availability` value, matching Chromium's
/// `DeveloperToolsAvailability::kDisallowedForForceInstalledExtensions` — DevTools are
/// available everywhere but on a force-installed extension, which a CEF application has
/// none of. `2` is the value that forbids them outright.
pub(crate) const DEVTOOLS_ALLOWED: i64 = 0;

/// `devtools.remote_debugging.allowed`, the local-state preference
/// `RemoteDebuggingServer::GetInstance` consults before it will start a server for
/// `--remote-debugging-port` or `--remote-debugging-pipe`.
pub(crate) const REMOTE_DEBUGGING_ALLOWED: &str = "devtools.remote_debugging.allowed";

/// Chromium profile preferences forced off for every webview, with the reason
/// each one is unwanted in an application webview:
///
/// * `credentials_enable_service` - Chrome offers to save credentials typed
///   into any form; an app's login form is not the browser's business.
/// * `profile.password_manager_leak_detection` - on by default in Chromium, it
///   sends a hashed prefix of credentials typed into any form to Google to check
///   them against known breaches.
/// * `autofill.profile_enabled` / `autofill.credit_card_enabled` - the same
///   deal for postal addresses and payment cards, which additionally sync into
///   the user's Google account.
/// * `translate.enabled` - the translate bubble both covers app UI and ships
///   page text off to Google's translation service to decide whether to offer.
/// * `alternate_error_pages.enabled` - on a failed navigation Chrome sends the
///   URL that failed to Google to fetch suggestions for it.
/// * `search.suggest_enabled` - streams typed input to the profile's default
///   search engine; an app has no omnibox for this to serve.
/// * `privacy_sandbox.m1.*` - the Topics, Protected Audience and attribution
///   reporting APIs. Chrome only turns these on after its own consent flow, which
///   never runs in CEF, so today they are already off; pinning them means a future
///   Chromium that flips the default cannot quietly enrol an application's users in
///   interest-based advertising.
///
/// `safebrowsing.enabled` is deliberately absent - see the module docs.
const PREFERENCES: &[(&str, bool)] = &[
  ("credentials_enable_service", false),
  ("profile.password_manager_leak_detection", false),
  ("autofill.profile_enabled", false),
  ("autofill.credit_card_enabled", false),
  ("translate.enabled", false),
  ("alternate_error_pages.enabled", false),
  ("search.suggest_enabled", false),
  ("privacy_sandbox.m1.topics_enabled", false),
  ("privacy_sandbox.m1.fledge_enabled", false),
  ("privacy_sandbox.m1.ad_measurement_enabled", false),
];

/// Converts a JSON value into the `CefValue` the preference API takes.
///
/// Chromium preferences are `base::Value`s of every shape - `proxy` is a dictionary,
/// `webrtc.ip_handling_policy` a string, `devtools.availability` an integer - so the
/// runtime carries them as [`serde_json::Value`] and converts here. Returns [`None`] when
/// CEF will not allocate, which is the only failure mode: every JSON shape has a
/// `base::Value` counterpart.
fn to_cef_value(value: &serde_json::Value) -> Option<Value> {
  let cef_value = cef::value_create()?;

  match value {
    serde_json::Value::Null => {
      cef_value.set_null();
    }
    serde_json::Value::Bool(value) => {
      cef_value.set_bool(i32::from(*value));
    }
    serde_json::Value::Number(number) => {
      // Chromium stores an integer preference as an int and refuses a double in its
      // place, so an integral JSON number has to stay integral. `as_i64` answers only
      // for numbers that really are integers.
      match number.as_i64() {
        Some(integer) => {
          let Ok(integer) = i32::try_from(integer) else {
            log::debug!("preference value {integer} does not fit in the int Chromium stores");
            return None;
          };
          cef_value.set_int(integer);
        }
        None => {
          cef_value.set_double(number.as_f64()?);
        }
      }
    }
    serde_json::Value::String(string) => {
      cef_value.set_string(Some(&CefString::from(string.as_str())));
    }
    serde_json::Value::Array(items) => {
      let list = cef::list_value_create()?;
      for (index, item) in items.iter().enumerate() {
        let mut item = to_cef_value(item)?;
        list.set_value(index, Some(&mut item));
      }
      let mut list = list;
      cef_value.set_list(Some(&mut list));
    }
    serde_json::Value::Object(entries) => {
      let dictionary = cef::dictionary_value_create()?;
      for (key, entry) in entries {
        let mut entry = to_cef_value(entry)?;
        dictionary.set_value(Some(&CefString::from(key.as_str())), Some(&mut entry));
      }
      let mut dictionary = dictionary;
      cef_value.set_dictionary(Some(&mut dictionary));
    }
  }

  Some(cef_value)
}

/// Applies [`PREFERENCES`], then `overrides`, to `request_context`.
///
/// Must be called after the request context has finished initializing - a
/// preference cannot be written before the underlying Chromium `Profile`
/// exists.
///
/// `overrides` are the application's own, from `Cef::profile_preference` and the typed
/// options that write one. They are written last so that naming a preference this module
/// disables turns it back on, and so that a repeated name keeps its last value.
pub(crate) fn apply_app_webview_preferences(
  request_context: &RequestContext,
  overrides: &[(String, serde_json::Value)],
) {
  let defaults = PREFERENCES
    .iter()
    .map(|(name, enabled)| ((*name).to_string(), serde_json::Value::Bool(*enabled)));

  for (name, value) in defaults.chain(overrides.iter().cloned()) {
    let _ = set_preference(request_context, &name, &value);
  }
}

/// Applies the application's default content settings to `request_context`.
///
/// A null URL pair is how CEF spells "the default for every origin", which is the
/// application-wide policy an embedder wants: a page cannot ask for a permission whose
/// default is `BLOCK`, and one whose default is `ALLOW` never prompts.
pub(crate) fn apply_default_content_settings(
  request_context: &RequestContext,
  settings: &[(ContentSettingTypes, ContentSettingValues)],
) {
  for (content_type, value) in settings {
    request_context.set_content_setting(None, None, *content_type, *value);
  }
}

/// Writes one preference on any store that has a preference manager, skipping it when
/// this Chrome build will not take it. Returns whether it was written.
///
/// Which preferences a given Chrome build registers as writable varies, and there is one
/// request context per webview, so a refused preference is logged at debug rather than
/// warned about. A caller for whom the write is the whole point — the proxy is the
/// standing case — checks the return value and says more.
#[must_use]
pub(crate) fn set_preference<M: ImplPreferenceManager>(
  manager: &M,
  name: &str,
  value: &serde_json::Value,
) -> bool {
  if manager.can_set_preference(Some(&name.into())) != 1 {
    log::debug!("the CEF preference store does not allow setting the {name} preference");
    return false;
  }

  let Some(value) = to_cef_value(value) else {
    log::debug!("failed to build a CEF value for the {name} preference");
    return false;
  };

  let mut value = value;
  let mut error = set_preference_error_slot();
  if manager.set_preference(Some(&name.into()), Some(&mut value), Some(&mut error)) != 1 {
    log::debug!("failed to apply the {name} preference: {error}");
    return false;
  }

  true
}

/// Applies the preferences that live in Chromium's local state rather than in a profile.
///
/// Called once the CEF context is initialized, which is when
/// `cef::preference_manager_get_global()` starts answering.
pub(crate) fn apply_global_preferences(overrides: &[(String, serde_json::Value)]) {
  if overrides.is_empty() {
    return;
  }

  let Some(manager) = cef::preference_manager_get_global() else {
    log::debug!("the global CEF preference manager is unavailable; skipping global preferences");
    return;
  };

  for (name, value) in overrides {
    let _ = set_preference(&manager, name, value);
  }
}

/// Writes a boolean local-state preference into the profile's `Local State` file before
/// `cef::initialize` runs.
///
/// [`apply_global_preferences`] can only run once the CEF context exists, but Chromium
/// reads some of these while `cef::initialize` starts up: it decides whether to start the
/// DevTools server from `devtools.remote_debugging.allowed` as stored by the previous
/// launch. Without this, a change would only take effect one launch late. A missing or
/// unreadable file is left alone: Chromium then starts from its default, and the
/// in-process write still follows.
pub(crate) fn store_local_state_preference(
  cache_path: &std::path::Path,
  name: &str,
  value: bool,
) -> std::io::Result<()> {
  let path = cache_path.join("Local State");
  let Ok(contents) = std::fs::read_to_string(&path) else {
    return Ok(());
  };
  let Ok(mut state) = serde_json::from_str::<serde_json::Value>(&contents) else {
    return Ok(());
  };

  let mut node = &mut state;
  for key in name.split('.') {
    let Some(object) = node.as_object_mut() else {
      return Ok(());
    };
    node = object
      .entry(key)
      .or_insert_with(|| serde_json::Value::Object(Default::default()));
  }
  if node.as_bool() == Some(value) {
    return Ok(());
  }
  *node = serde_json::Value::Bool(value);

  let temp = path.with_extension("tmp");
  std::fs::write(&temp, serde_json::to_vec(&state)?)?;
  std::fs::rename(&temp, &path)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn the_disabled_preferences_are_all_disabled() {
    // Every entry exists to turn something off; an entry set to `true` would be a
    // default this module has no business asserting.
    for (name, enabled) in PREFERENCES {
      assert!(!enabled, "{name} is listed as a preference forced off");
    }
  }

  #[test]
  fn safe_browsing_is_not_disabled_by_default() {
    assert!(
      !PREFERENCES
        .iter()
        .any(|(name, _)| *name == "safebrowsing.enabled"),
      "Safe Browsing stays on unless the application asks otherwise; see the module docs"
    );
  }

  #[test]
  fn the_privacy_sandbox_apis_are_pinned_off() {
    for api in [
      "privacy_sandbox.m1.topics_enabled",
      "privacy_sandbox.m1.fledge_enabled",
      "privacy_sandbox.m1.ad_measurement_enabled",
    ] {
      assert!(PREFERENCES.iter().any(|(name, _)| *name == api));
    }
  }

  fn local_state_dir(test: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("tauri-runtime-cef-{test}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
  }

  fn read_local_state(dir: &std::path::Path) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(dir.join("Local State")).unwrap()).unwrap()
  }

  #[test]
  fn a_stored_value_is_overwritten_before_startup() {
    let dir = local_state_dir("overwrite");
    std::fs::write(
      dir.join("Local State"),
      r#"{"devtools":{"remote_debugging":{"allowed":false}},"other":1}"#,
    )
    .unwrap();

    store_local_state_preference(&dir, REMOTE_DEBUGGING_ALLOWED, true).unwrap();

    let state = read_local_state(&dir);
    assert_eq!(state["devtools"]["remote_debugging"]["allowed"], true);
    assert_eq!(state["other"], 1);
  }

  #[test]
  fn missing_keys_are_created() {
    let dir = local_state_dir("create");
    std::fs::write(dir.join("Local State"), "{}").unwrap();

    store_local_state_preference(&dir, REMOTE_DEBUGGING_ALLOWED, false).unwrap();

    assert_eq!(
      read_local_state(&dir)["devtools"]["remote_debugging"]["allowed"],
      false
    );
  }

  #[test]
  fn a_missing_local_state_is_left_to_chromium() {
    let dir = local_state_dir("missing");

    store_local_state_preference(&dir, REMOTE_DEBUGGING_ALLOWED, true).unwrap();

    assert!(!dir.join("Local State").exists());
  }
}
