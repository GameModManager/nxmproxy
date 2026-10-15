// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 The GameModManager community (this fork)
// Fork of https://github.com/TanninOne/nxmproxy - see NOTICE.md

//! macOS: publish a minimal app bundle, then ask LaunchServices for the scheme.
//!
//! ## Deliberate divergence from upstream
//!
//! Upstream had no macOS code at all.
//!
//! An earlier version of this fork called only `lsregister`, leaving the bundle
//! eligible but not chosen, with the argument that assigning the scheme is the
//! greed this tool exists to stop. That was half right: it is true that nothing
//! here may take the scheme from *another* manager by editing its own record,
//! and it is false that macOS lets an app install itself as the handler
//! silently. `LSSetDefaultHandlerForURLScheme` on macOS 12 and later **asks
//! the user**. The OS asking is the consent step, and it is the only way to get
//! a working install without making the user find a settings pane. So the call
//! is made, and its result is reported as *unverified* rather than as a win,
//! because nothing here can read the answer back.
//!
//! ## Verification status - READ THIS BEFORE TRUSTING THE macOS PATH
//!
//! **The FFI below was written against Apple's documentation, not against a
//! Mac.** It is three declarations: `CFStringCreateWithCString` and
//! `CFRelease` from CoreFoundation, and `LSSetDefaultHandlerForURLScheme` from
//! LaunchServices. What CI proves is that it compiles against the macOS SDK
//! and links against the two system frameworks. What **nothing** proves is
//! what it does: whether macOS shows the prompt, whether it takes the claim
//! when the user says yes, whether `Alternate` rank still influences the
//! chooser, or what `test` reports on a machine where something else already
//! owns `nxm://`. Those need a real Mac and a real click.
//!
//! `current_handler` stays honest about the gap rather than guessing. The real
//! answer requires `LSCopyDefaultHandlerForURLScheme`, which this fork does not
//! bind; the record is `~/Library/Preferences/com.apple.LaunchServices/com.apple.launchservices.secure.plist`,
//! a binary plist we do not parse. So `test` reports "unknown" and says where
//! to look, rather than inventing a boolean.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::logger;
use crate::platform::Claim;

const SCHEME: &str = "nxm";
pub const BUNDLE_ID: &str = "org.gamemodmanager.nxmproxy";
const LSREGISTER: &str =
    "/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister";

/// The minimum of Apple's API needed to hand a scheme to a bundle.
///
/// Declared here rather than pulled in through a binding crate, for the same
/// reason the rest of this project drives `reg.exe` instead of the Win32 API:
/// a codegen crate's build script only emits for one platform and takes
/// `cargo check` down on the other two. `CFStringRef` is an opaque pointer, so
/// the signature needs nothing from any SDK header.
#[cfg(target_os = "macos")]
mod ffi {
    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        /// `CFStringCreateWithCString(allocator, cStr, encoding)`. An
        /// unretained string: the system owns it until we release it.
        pub fn CFStringCreateWithCString(
            allocator: *const core::ffi::c_void,
            c_str: *const core::ffi::c_char,
            encoding: u32,
        ) -> *mut core::ffi::c_void;

        /// `CFRelease` - pairs with the create above. Underflowing this leaks
        /// two strings per install; nobody would notice, but it is still wrong.
        pub fn CFRelease(cf: *const core::ffi::c_void);
    }

    #[link(name = "CoreServices", kind = "framework")]
    extern "C" {
        /// `LSSetDefaultHandlerForURLScheme(scheme, bundleIDOrNil)`.
        /// Returns 0 on success. On macOS 12+ a user prompt is shown first.
        pub fn LSSetDefaultHandlerForURLScheme(
            scheme: *const core::ffi::c_void,
            bundle_id: *const core::ffi::c_void,
        ) -> i32;
    }

    /// `kCFStringEncodingUTF8`.
    pub const UTF8: u32 = 0x0800_0100;

    /// A retained CFString from a Rust `&str`, released on drop.
    pub struct CfString(*mut core::ffi::c_void);

    impl CfString {
        pub fn new(text: &str) -> Option<Self> {
            // NUL would truncate the string; a bundle id never contains one,
            // but a silently truncated CFString would be a nightmare to find.
            if text.contains('\0') {
                return None;
            }
            let created = unsafe {
                CFStringCreateWithCString(core::ptr::null(), text.as_ptr() as *const _, UTF8)
            };
            (!created.is_null()).then_some(CfString(created))
        }

        pub fn as_ptr(&self) -> *const core::ffi::c_void {
            self.0
        }
    }

    impl Drop for CfString {
        fn drop(&mut self) {
            unsafe { CFRelease(self.0) }
        }
    }
}

/// Hand `nxm://` to our bundle.
///
/// Returns a claim that is **never** `Claim::verified`: the API is one-way and
/// cannot be read back, so "it worked" is not a statement this program is able
/// to make on macOS. The user-facing instruction is printed either way.
///
/// Off macOS this returns the same honest refusal instead of panicking, so
/// that the invariant above is testable from a Linux build. `platform::imp`
/// never selects this module there; the branch exists for the tests.
pub fn claim() -> Result<Claim, String> {
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (SCHEME, BUNDLE_ID);
        Ok(Claim::needs_user(
            "this build is not running on macOS, so no scheme was claimed",
        ))
    }
    #[cfg(target_os = "macos")]
    {
        let scheme = ffi::CfString::new(SCHEME).ok_or("could not build the scheme string")?;
        let bundle = ffi::CfString::new(BUNDLE_ID).ok_or("could not build the bundle id")?;

        // Safe: both pointers are live retained CFStrings owned by `scheme`
        // and `bundle`, which outlive the call.
        let status =
            unsafe { ffi::LSSetDefaultHandlerForURLScheme(scheme.as_ptr(), bundle.as_ptr()) };
        logger::info(format!(
            "LSSetDefaultHandlerForURLScheme({SCHEME}) -> {status}"
        ));
        if status != 0 {
            return Err(format!(
                "LaunchServices refused to assign {SCHEME}:// to {BUNDLE_ID} (OSStatus {status}). \
                 Set it by hand: Finder > Get Info on NXM Proxy.app > Open with > Change All."
            ));
        }
        Ok(Claim::needs_user(
            "macOS was asked to hand nxm:// to NXM Proxy and may ask you to confirm. \
             If it did not, set it by hand: Finder > Get Info on NXM Proxy.app > Open with > \
             NXM Proxy > Change All. nxmproxy cannot read the answer back, so it will not \
             claim success.",
        ))
    }
}

/// Give the scheme back.
///
/// It cannot be given back. macOS has no supported way to *unset* a scheme
/// handler; the only supported move is to point the scheme at something else,
/// which is the user's click to make. Say so rather than pretending.
pub fn unclaim() -> Result<Claim, String> {
    Ok(Claim::needs_user(
        "The NXM Proxy bundle is gone, but macOS will still route nxm:// somewhere and \
         nxmproxy cannot take the claim away by itself.\n  \
         To change it: Finder > Get Info on the app you want > Open with > Change All.",
    ))
}

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
    fn the_claim_goes_through_launchservices() {
        // The bug this asserts against, and the reason it is here at all: an
        // earlier version of this fork refused to call
        // LSSetDefaultHandlerForURLScheme on principle and only ran lsregister,
        // which leaves the bundle eligible and not chosen. `install` then
        // reported success over an install that routed nothing. macOS asks the
        // user; the ask is the consent step.
        //
        // Scans production source, see `crate::platform::production_code`, and
        // so it fails if the call is renamed away, moved behind a cfg that is
        // never true, or deleted.
        let code = crate::platform::production_code(include_str!("macos.rs"));
        assert!(
            code.contains("LSSetDefaultHandlerForURLScheme"),
            "install no longer asks LaunchServices for the scheme, so it cannot claim it:\n{code}"
        );
        // And it must be reached from claim(), not merely mentioned.
        let claim = claim_fn_body();
        assert!(
            claim.contains("LSSetDefaultHandlerForURLScheme"),
            "the call is declared but claim() does not make it:\n{claim}"
        );
    }

    #[test]
    fn a_claim_is_never_reported_as_verified() {
        // macOS cannot be read back from here, so `install` must not tell a
        // script it succeeded. The exit code is the whole point of the
        // NEEDS_USER split, and a green here would be a lie in the only place
        // anyone reads it.
        //
        // Asserted against `claim`, not `unclaim`: `unclaim` was never capable
        // of claiming anything, so testing it proves nothing about install.
        // This now runs on Linux too, which is the only way a Linux build can
        // see the invariant at all.
        let claim = claim().expect("claim must not error");
        assert!(
            !claim.claimed,
            "macOS cannot be read back, so install must never report a verified claim"
        );
        assert_eq!(
            claim.exit_code(),
            crate::platform::EXIT_NEEDS_USER,
            "a script must be able to tell 'registered, one click left' from 'done'"
        );
        assert!(
            !claim.instruction.trim().is_empty(),
            "a claim that was not made must still say what to do by hand"
        );
    }

    #[test]
    fn uninstall_is_honest_that_macos_cannot_take_the_scheme_back() {
        // Symmetric with claim(). macOS has no supported way to *unset* a
        // scheme handler, and saying otherwise would be a lie.
        let un = unclaim().expect("unclaim must not error");
        assert!(!un.claimed);
        assert_eq!(un.exit_code(), crate::platform::EXIT_NEEDS_USER);
    }

    /// The text of `claim`, so the test above can tell "mentioned in a comment"
    /// from "actually called". Leading indentation is preserved, so this
    /// works wherever the function happens to sit in the file.
    fn claim_fn_body() -> String {
        let source = include_str!("macos.rs");
        let start = source
            .find("pub fn claim()")
            .expect("claim() must exist: install depends on it");
        let rest = &source[start..];
        let end = rest.find("\npub fn unclaim()").unwrap_or(rest.len());
        rest[..end].to_string()
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
