// Copyright 2019-2024 Tauri Programme within The Commons Conservancy
// SPDX-License-Identifier: Apache-2.0
// SPDX-License-Identifier: MIT

use std::{
  fs,
  io::Read,
  path::{Path, PathBuf},
  process::Command,
};

use anyhow::Context;
use walkdir::WalkDir;

use crate::{
  Settings,
  bundle::{linux::freedesktop, settings::Arch},
  utils::{
    fs_utils,
    http_utils::{download_and_verify, verify_file_hash, HashAlgorithm},
    CommandExt,
  },
};

use super::write_and_make_executable;

/// Pinned revision of the Anylinux-AppImages tooling.
///
/// `quick-sharun.sh` drives the entire deployment and upstream moves fast, so we
/// pin and checksum it instead of tracking a branch: a release build must not
/// change behaviour because of a push nobody reviewed. Bump both constants
/// together after testing the new revision. Includes the deployment-array
/// quoting fix from pkgforge-dev/Anylinux-AppImages#855.
///
/// The script still downloads a few tools of its own (sharun, appimagetool,
/// uruntime, onelf); the ones it pins itself are pinned, the rest track their
/// latest release.
const QUICK_SHARUN_REV: &str = "53a05bc37f0d5241fe7ce3c7daee20fd93e26750";
const QUICK_SHARUN_SHA256: &str =
  "4a24616f07ff4e9ab908ef17d5ae6e32fe4b697aa21bcd1cd956032648a553d9";

fn anylinux_raw_url(file: &str) -> String {
  format!(
    "https://raw.githubusercontent.com/pkgforge-dev/Anylinux-AppImages/{QUICK_SHARUN_REV}/useful-tools/{file}"
  )
}

/// Resolves the pinned `quick-sharun.sh`, downloading it only when the cached
/// copy is missing or does not match the pinned checksum. Keeping the revision
/// in the file name means a build is reproducible and, after the first run,
/// works offline.
fn quick_sharun_script(tools_path: &Path) -> crate::Result<PathBuf> {
  let script = tools_path.join(format!("quick-sharun-{}.sh", &QUICK_SHARUN_REV[..12]));

  if verify_file_hash(&script, QUICK_SHARUN_SHA256, HashAlgorithm::Sha256).is_ok() {
    return Ok(script);
  }

  let data = download_and_verify(
    &anylinux_raw_url("quick-sharun.sh"),
    QUICK_SHARUN_SHA256,
    HashAlgorithm::Sha256,
  )?;
  write_and_make_executable(&script, data)?;

  Ok(script)
}

// TODO: Test if bundling xdg-mime makes sense (eg does it even work if it's not on the host system?)
// TODO: Monitor TLS support / certificates - seems to be working in initial tests
pub fn bundle_project(settings: &Settings) -> crate::Result<Vec<PathBuf>> {
  // for backwards compat we keep the amd64 and i386 rewrites in the filename
  let appimage_arch = match settings.binary_arch() {
    Arch::X86_64 => "amd64",
    //Arch::X86 => "i386",
    Arch::AArch64 => "aarch64",
    //Arch::Armhf => "armhf",
    target => {
      return Err(crate::Error::ArchError(format!(
        "Unsupported architecture: {target:?}"
      )));
    }
  };
  //let tools_arch = settings.target().split('-').next().unwrap();

  let output_path = settings.project_out_directory().join("bundle/appimage");
  if output_path.exists() {
    fs::remove_dir_all(&output_path)?;
  }

  let product_name = settings.product_name();

  let appimage_filename = format!(
    "{}_{}_{appimage_arch}.AppImage",
    product_name,
    settings.version_string()
  );
  let appimage_path = output_path.join(&appimage_filename);

  let tools_path = settings
    .local_tools_directory()
    .map(|d| d.join(".tauri"))
    .unwrap_or_else(|| {
      dirs::cache_dir().map_or_else(|| output_path.to_path_buf(), |p| p.join("tauri"))
    });

  fs::create_dir_all(&tools_path)?;

  let quick_sharun = quick_sharun_script(&tools_path)?;

  // This should come after the download or users will think it's stuck on the download step.
  log::info!(action = "Bundling"; "{} ({})", appimage_filename, appimage_path.display());

  let mut settings = settings.clone();
  if settings.main_binary()?.name().contains(' ') {
    let main_binary = settings.main_binary()?;

    let main_binary_path = settings.binary_path(main_binary);
    let project_out_dir = settings.project_out_directory();

    let main_binary_name_kebab = heck::AsKebabCase(main_binary.name()).to_string();
    let new_path = project_out_dir.join(&main_binary_name_kebab);
    fs::copy(main_binary_path, new_path)?;

    let main_binary = settings.main_binary_mut()?;
    main_binary.set_name(main_binary_name_kebab);
  }
  let settings = settings;

  fs::create_dir_all(&output_path)?;
  // quick-sharun rebuilds its argument list through `eval`, which splits a
  // path on whitespace, and it silently skips wrapping the application binary
  // when the AppDir path contains any. Name the AppDir without whitespace;
  // the desktop entry and icon below keep the real product name.
  let app_dir_name = product_name
    .chars()
    .map(|c| if c.is_whitespace() { '_' } else { c })
    .collect::<String>();
  let app_dir = output_path.join(format!("{app_dir_name}.AppDir"));
  if app_dir.to_string_lossy().contains(char::is_whitespace) {
    return Err(crate::Error::GenericError(format!(
      "cannot bundle an AppImage under a path that contains whitespace: {}. quick-sharun would split it and skip deploying the application binary. Build from a path without whitespace.",
      app_dir.display()
    )));
  }
  let app_dir_bin = app_dir.join("bin/");
  let app_dir_lib = app_dir.join("lib/");

  let desktop_file = freedesktop::generate_desktop_file(&settings, &None, &app_dir)
    .with_context(|| "Failed to create desktop file")?
    .0;
  fs::rename(
    desktop_file,
    app_dir.join(format!("{product_name}.desktop")),
  )
  .with_context(|| "Failed to move desktop file")?;
  let _ = fs_utils::remove_dir_all(&app_dir.join("usr/"));

  // Copy Cargo project binaries
  for bin in settings.binaries() {
    let bin_path = settings.binary_path(bin);
    let trgt = app_dir_bin.join(bin.name());
    fs_utils::copy_file(&bin_path, &trgt)
      .with_context(|| format!("Failed to copy binary from {bin_path:?} to {trgt:?}"))?;
  }

  // Copy external binaries (externalBin)
  settings
    .copy_binaries(&app_dir_bin)
    .with_context(|| "Failed to copy external binaries")?;

  settings
    .copy_resources(&app_dir_lib.join(product_name))
    .with_context(|| "Failed to copy resource files")?;

  fs_utils::copy_custom_files(&settings.appimage().files, &app_dir)
    .with_context(|| "Failed to copy custom files")?;

  let icons = freedesktop::list_icon_files(&settings, Path::new(""))
    .with_context(|| "Failed to create icon files")?;

  let largest_icon = icons
    .into_iter()
    .filter(|(i, _)| i.width == i.height)
    .max_by_key(|(i, _)| i.width)
    .expect("couldn't find a square icon to use as AppImage icon");

  fs::copy(largest_icon.1, app_dir.join(format!("{product_name}.png")))
    .with_context(|| "Failed to copy icon file")?;

  // `None` on a shared runtime: the app resolves and loads CEF from outside
  // the bundle at launch, so there is nothing to copy in. Chromium's host
  // dependencies are still deployed below (DEPLOY_CHROMIUM), because the
  // libcef the app loads resolves them against this bundle.
  if let Some(cef_path) = settings.webview_runtime().cef_distribution() {
    fs::create_dir_all(app_dir_bin.join("locales/"))?;

    let cef_files = [
      // required
      "libcef.so",
      "icudtl.dat",
      "v8_context_snapshot.bin",
      // required end
      // "optional" - but not really since we want support for all of this
      "chrome_100_percent.pak",
      "chrome_200_percent.pak",
      "resources.pak",
      // ANGLE support
      "libEGL.so",
      "libGLESv2.so",
      // SwANGLE support
      "libvk_swiftshader.so",
      "vk_swiftshader_icd.json",
      "libvulkan.so.1",
      // sandbox - may need to be behind a setting?
      "chrome-sandbox",
      // TODO: seccomp
    ];

    for f in cef_files {
      let dest = app_dir_bin.join(f);
      fs::copy(cef_path.join(f), &dest)
        .with_context(|| format!("Failed to copy cef file {f} to {}", dest.display()))?;
      // quick-sharun checks for the NO_STRIP env but libcef.so is 1.5GB so we make sure it's stripped anyway.
      let _ = Command::new("strip").arg(&dest).output_ok();
    }
    let locales = [
      "en-US.pak",
      "en-US_FEMININE.pak",
      "en-US_MASCULINE.pak",
      "en-US_NEUTER.pak",
    ];

    for f in locales {
      fs::copy(
        cef_path.join("locales").join(f),
        app_dir_bin.join("locales").join(f),
      )
      .with_context(|| format!("Failed to copy cef locales file {f}"))?;
    }
  }

  // We need to give quick-sharun the list of binaries AND libraries to include.
  // To support weird `appimage.files` settings we just walk through the whole AppDir we set up.
  // TODO: In some cases we may have to give quick-sharun the path to some directories as well.
  let mut elfs: Vec<String> = Vec::new();
  for entry in WalkDir::new(&app_dir) {
    if let Ok(entry) = entry
      && entry.file_type().is_file()
      && is_elf(entry.path())
    {
      elfs.push(entry.path().to_string_lossy().to_string());
    }
  }
  // This is mostly for libappindicator that we added to /usr/lib in tauri-cli/src/interface/rust.rs
  for (target, source) in &settings.appimage().files {
    if target.starts_with("/usr/lib") {
      elfs.push(source.to_string_lossy().to_string());
    }
  }
  // `files` is a HashMap, so sort to keep the command reproducible.
  elfs.sort();

  // quick-sharun runs each binary it deploys for a few seconds to see which
  // libraries get dlopened, then kills it with a process-group signal. That
  // only reaches the process if the shell put it in its own group, which is
  // what `set -m` is for. dash does not create the group when there is no
  // controlling terminal, so on Debian and Ubuntu, where /bin/sh is dash,
  // every terminal-less build - which is every CI run - hangs forever on the
  // first traced process that does not exit by itself. bash creates the group
  // either way, so prefer it and fall back to sh where it is missing.
  let shell = ["/bin/bash", "/usr/bin/bash"]
    .into_iter()
    .find(|p| Path::new(p).exists())
    .unwrap_or("/bin/sh");

  // Passing the script to the shell as an argument rather than building a
  // `-c` string keeps paths containing spaces intact.
  let mut cmd = Command::new(shell);
  cmd
    .arg(&quick_sharun)
    .args(&elfs)
    .current_dir(&output_path)
    .env("APPDIR", &app_dir)
    // At least on my local machine this was required, worked fine without in CI / using published tauri-apps/cli-cef.
    .env("MAIN_BIN", app_dir_bin.join(settings.main_binary()?.name()))
    .env("OUTPUT_APPIMAGE", "1")
    .env("OUTNAME", &appimage_filename)
    // Pin the helper library the script compiles into the bundle to the same
    // revision as the script itself.
    .env("ANYLINUX_LIB_SOURCE", anylinux_raw_url("lib/anylinux.c"))
    // Chromium's own host dependencies: NSS, pulse via pipewire, GL, p11-kit.
    // No ADD_HOOKS: the fix-namespaces hook asks for a root password to
    // enable unprivileged user namespaces, which CEF only needs when it is
    // built with the sandbox feature, and it is not on Linux.
    .env("DEPLOY_CHROMIUM", "1");

  // quick-sharun's strace mode runs the app for a few seconds and deploys every
  // library it sees loaded. On a shared runtime that means the CEF distribution
  // the app resolves at launch — the one thing this bundle exists not to carry —
  // gets copied back in, unstripped. Its discovery filter is hardcoded, so the
  // only way to leave it out is to skip the tracing; the DEPLOY_* rules above
  // still cover GTK, OpenGL, Vulkan, NSS and the other Chromium host libraries.
  if settings.webview_runtime().cef_distribution().is_none() {
    cmd.env("STRACE_MODE", "0");
  }

  // Streams the tooling's output instead of capturing it: this runs for
  // minutes, downloads tools and launches the app, and its own error messages
  // are the only useful diagnostics when something is missing on the system.
  let status = cmd.piped().context("Failed to run quick-sharun")?;
  if !status.success() {
    return Err(crate::Error::GenericError(
      "quick-sharun failed to build the AppImage, see the output above for details".into(),
    ));
  }

  if !appimage_path.exists() {
    return Err(crate::Error::GenericError(format!(
      "quick-sharun did not produce {}",
      appimage_path.display()
    )));
  }

  Ok(vec![appimage_path])
}

fn is_elf(path: &Path) -> bool {
  let mut buf = [0; 4];
  if let Ok(mut file) = fs::File::open(path)
    && file.read_exact(&mut buf).is_ok()
  {
    return buf == [0x7f, b'E', b'L', b'F'];
  }
  false
}
