// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 The GameModManager community (this fork)
// Fork of https://github.com/TanninOne/nxmproxy - see NOTICE.md

//! macOS: publish a minimal app bundle and let LaunchServices know about it.
//!
//! ## Deliberate divergence from upstream
//!
//! Upstream had no macOS code at all. The obvious implementation - call
//! `LSSetDefaultHandlerForURLScheme` - **forces** the claim: there is no
//! "suggested", it assigns the scheme to our bundle outright. That is the same
//! greed as `xdg-mime default`, and it buries any other manager. We call
//! `lsregister` instead, which makes the bundle *eligible*. macOS then asks,
//! or the user picks in System Settings.
//!
//! ## Verification status
//!
//! Written on a machine with no macOS SDK, so it was never run there by the
//! author. Since added to CI it **compiles** on `aarch64-apple-darwin` and the
//! unit tests pass on `macos-latest`: the plist text, the launcher script and
//! the shell quoting are tested, because this file contains no FFI and only
//! writes text files.
//!
//! What is **not** verified is LaunchServices' actual behaviour: whether the
//! chooser appears, in which order the entries are offered, whether `Alternate`
//! rank does what is intended, and what `test` reports on a machine where
//! another manager already owns `nxm://`. That needs a real Mac and a real
//! click.
//!
//! `current_handler` is honest about the gap rather than guessing. The real
//! answer requires `LSCopyDefaultHandlerForURLScheme`, a CoreFoundation call
//! this fork deliberately does not bind; it is backed by
//! `~/Library/Preferences/com.apple.LaunchServices/com.apple.launchservices.secure.plist`,
//! a binary plist we do not parse. So `test` reports "unknown" and says where
//! to look, rather than inventing a boolean.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::logger;

const SCHEME: &str = "nxm";
pub const BUNDLE_ID: &str = "org.gamemodmanager.nxmproxy";
const LSREGISTER: &str =
    "/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister";

/// The `Info.plist` for the bundle. Pure, so its exact contents are testable.
///
/// `CFBundleURLTypes` is what tells LaunchServices the bundle handles `nxm`.
/// `LSHandlerRank` is deliberately left as `Alternate`: `Owner` and `Default`
/// both push us up the list, and pushing is the thing we are not doing.
///
/// The executable is not named here. It lives in the launcher script under
/// `Contents/MacOS`, so the bundle keeps working if this binary is moved.
pub fn info_plist() -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleName</key>
	<string>NXM Proxy</string>
	<key>CFBundleDisplayName</key>
	<string>NXM Proxy</string>
	<key>CFBundleIdentifier</key>
	<string>{BUNDLE_ID}</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleExecutable</key>
	<string>NXMProxy</string>
	<key>CFBundleURLTypes</key>
	<array>
		<dict>
			<key>CFBundleURLName</key>
			<string>{BUNDLE_ID}.nxm</string>
			<key>CFBundleTypeRole</key>
			<string>Viewer</string>
			<key>CFBundleURLSchemes</key>
			<array>
				<string>{SCHEME}</string>
			</array>
		</dict>
	</array>
	<key>LSHandlerRank</key>
	<string>Alternate</string>
</dict>
</plist>
"#
    )
}

/// Where the bundle lives. Not in `/Applications`: we are not asking for
/// elevation or for the user to install us as a real application.
pub fn bundle_path() -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| "could not determine the home directory".to_string())?;
    Ok(home
        .join("Library")
        .join("Application Support")
        .join("nxmproxy")
        .join("NXMProxy.app"))
}

/// The launcher inside `Contents/MacOS`. A shell script, so the bundle is a
/// valid executable without needing a Mach-O copy of ourselves.
///
/// The url arrives as `$1` and is forwarded untouched, so a url containing a
/// quote or a space survives: the shell already parsed it into one argument
/// before we ever see it.
fn launcher_script(exe: &Path) -> String {
    format!(
        "#!/bin/sh\nexec {} url \"$1\"\n",
        shell_quote(&exe.to_string_lossy())
    )
}

/// Quote for `/bin/sh`. Single quotes, with the standard `'\''` escape, which
/// is exact for any byte except NUL.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

pub fn install() -> Result<(), String> {
    let exe = std::env::current_exe()
        .map_err(|e| format!("could not determine this executable's path: {e}"))?;
    let bundle = bundle_path()?;
    let macos_dir = bundle.join("Contents").join("MacOS");

    std::fs::create_dir_all(&macos_dir)
        .map_err(|e| format!("could not create {}: {}", macos_dir.display(), e))?;
    std::fs::write(bundle.join("Contents").join("Info.plist"), info_plist())
        .map_err(|e| format!("could not write Info.plist: {e}"))?;

    let launcher = macos_dir.join("NXMProxy");
    std::fs::write(&launcher, launcher_script(&exe))
        .map_err(|e| format!("could not write {}: {}", launcher.display(), e))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("could not make {} executable: {}", launcher.display(), e))?;
    }

    // Registration only. LSSetDefaultHandlerForURLScheme would force the claim
    // and is deliberately not called.
    let output = Command::new(LSREGISTER)
        .arg("-f")
        .arg(&bundle)
        .output()
        .map_err(|e| format!("could not run lsregister ({e}); is this macOS?"))?;
    if !output.status.success() {
        return Err(format!(
            "lsregister failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    logger::info(format!(
        "registered {BUNDLE_ID} as an nxm:// handler. macOS may still ask which app to use; \
         that is intended."
    ));
    Ok(())
}

pub fn uninstall() -> Result<(), String> {
    let bundle = bundle_path()?;
    if bundle.exists() {
        // Best effort: unregistering should not fail because lsregister is
        // unhappy, and the bundle is going away either way.
        let _ = Command::new(LSREGISTER).arg("-u").arg(&bundle).status();
        std::fs::remove_dir_all(&bundle)
            .map_err(|e| format!("could not remove {}: {}", bundle.display(), e))?;
        logger::info(format!("removed {}", bundle.display()));
    }
    Ok(())
}

pub fn is_registered() -> Result<bool, String> {
    Ok(bundle_path()?.join("Contents").join("Info.plist").exists())
}

/// Cannot report the default handler without binding
/// `LSCopyDefaultHandlerForURLScheme`, which this fork does not do. Says so.
pub fn current_handler() -> Result<Option<String>, String> {
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_plist_declares_the_nxm_scheme() {
        let plist = info_plist();
        assert!(plist.contains("<string>nxm</string>"), "{plist}");
        assert!(plist.contains("CFBundleURLTypes"));
        assert!(plist.contains(BUNDLE_ID));
        assert!(plist.starts_with("<?xml"));
    }

    #[test]
    fn the_handler_rank_is_not_owner_or_default() {
        // Owner or Default would push us to the top of the chooser. That is
        // the behaviour we are refusing.
        let plist = info_plist();
        assert!(
            plist.contains("<key>LSHandlerRank</key>\n\t<string>Alternate</string>"),
            "{plist}"
        );
        assert!(!plist.contains("<string>Owner</string>"));
        assert!(!plist.contains("<string>Default</string>"));
    }

    #[test]
    fn we_never_call_the_forcing_api() {
        // LSSetDefaultHandlerForURLScheme is the macOS equivalent of
        // `xdg-mime default`: it assigns the scheme outright. Scans production
        // source, see `crate::platform::production_code`.
        let code = crate::platform::production_code(include_str!("macos.rs"));
        assert!(
            !code.contains("LSSetDefaultHandlerForURLScheme"),
            "the macOS forcing API may not be called:\n{code}"
        );
        assert!(
            !code.contains("lssetdefault"),
            "the macOS forcing API may not be spelled in shorthand either"
        );
    }

    #[test]
    fn the_launcher_forwards_the_url_as_one_argument() {
        let script = launcher_script(Path::new("/opt/My Mods/nxmproxy"));
        assert!(script.starts_with("#!/bin/sh\n"));
        assert!(
            script.contains("'/opt/My Mods/nxmproxy' url \"$1\""),
            "{script}"
        );
    }

    #[test]
    fn shell_quoting_handles_quotes_and_spaces() {
        assert_eq!(shell_quote("/usr/bin/nxmproxy"), "'/usr/bin/nxmproxy'");
        assert_eq!(shell_quote("/a b/nx"), "'/a b/nx'");
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
        assert_eq!(
            shell_quote("a;rm -rf /"),
            "'a;rm -rf /'",
            "no expansion, no substitution"
        );
    }
}
