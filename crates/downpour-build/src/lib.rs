//! Used by the build scripts of every Downpour executable, so all four carry
//! the same icon, version details, and application manifest on Windows.
//!
//! The manifest declares:
//! - Per-monitor (v2) DPI awareness. winit also asks for it at startup; with
//!   the manifest already setting it that call is refused, which winit
//!   ignores, so the two agree instead of conflicting.
//! - Common Controls v6, so system dialogs get the current look.
//! - Windows 10 and 11 as supported, so version checks see the real version.
//! - `asInvoker`: Downpour installs and runs per user, never elevated.
//! - UTF-8 as the ANSI code page, for any library that still calls the `A`
//!   functions. Rust's standard library uses the wide (`W`) functions, so
//!   this changes nothing for Downpour's own code.

use std::path::{Path, PathBuf};

pub const PRODUCT_NAME: &str = "Downpour";
/// Shared with the installer, which shows it as the publisher in Settings > Apps.
pub use downpour_protocol::desktop::PUBLISHER as COMPANY_NAME;
pub const COPYRIGHT: &str = "Copyright (c) 2026 Downpour contributors. MIT License.";

/// The icon every executable shows in Explorer and on the taskbar.
const ICON: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/downpour.ico");

/// One executable's resource details.
#[derive(Clone, Copy, Debug)]
pub struct Executable {
    /// The file name it is built as, for example `downpour.exe`.
    pub file_name: &'static str,
    /// What Task Manager and the file's Properties call it.
    pub description: &'static str,
    /// The manifest's assembly name, for example `Downpour.App`.
    pub assembly_name: &'static str,
}

/// A `major.minor.patch` version, as Cargo gives it to build scripts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Version {
    pub major: u16,
    pub minor: u16,
    pub patch: u16,
}

impl Version {
    /// Reads the package version Cargo passes to the running build script.
    fn of_package() -> Version {
        let part = |name: &str| -> u16 {
            let value = std::env::var(name).unwrap_or_else(|_| panic!("cargo sets {name}"));
            value
                .parse()
                .unwrap_or_else(|error| panic!("{name}={value} is not a version number: {error}"))
        };
        Version {
            major: part("CARGO_PKG_VERSION_MAJOR"),
            minor: part("CARGO_PKG_VERSION_MINOR"),
            patch: part("CARGO_PKG_VERSION_PATCH"),
        }
    }

    /// The four-part form manifests require, for example `1.0.0.0`.
    pub fn four_part(self) -> String {
        format!("{}.{}.{}.0", self.major, self.minor, self.patch)
    }
}

/// Embeds the icon, version details, and manifest into `executable` when
/// building for Windows. A build script calls this from `main`; it panics
/// with the reason when the resource compiler fails, because a build script
/// has no other way to report it.
pub fn embed_windows_resources(executable: Executable) {
    println!("cargo:rerun-if-changed={ICON}");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("cargo sets OUT_DIR"));
    let manifest_path = write_manifest(&out_dir, executable);

    let mut resource = winresource::WindowsResource::new();
    // winresource fills FileVersion and ProductVersion from the package
    // version already.
    resource
        .set_icon(ICON)
        .set_manifest_file(path_text(&manifest_path))
        .set("ProductName", PRODUCT_NAME)
        .set("FileDescription", executable.description)
        .set("CompanyName", COMPANY_NAME)
        .set("LegalCopyright", COPYRIGHT)
        .set("OriginalFilename", executable.file_name)
        .set("InternalName", executable.file_name);
    resource.compile().unwrap_or_else(|error| {
        panic!(
            "embed the Windows resources of {} (needs the Windows SDK's rc.exe): {error}",
            executable.file_name
        )
    });
}

fn write_manifest(out_dir: &Path, executable: Executable) -> PathBuf {
    let path = out_dir.join(format!("{}.manifest", executable.file_name));
    let text = manifest(executable.assembly_name, Version::of_package());
    std::fs::write(&path, text).unwrap_or_else(|error| panic!("write {}: {error}", path.display()));
    path
}

fn path_text(path: &Path) -> &str {
    path.to_str()
        .unwrap_or_else(|| panic!("{} is not valid Unicode", path.display()))
}

/// The application manifest for one executable.
pub fn manifest(assembly_name: &str, version: Version) -> String {
    let version = version.four_part();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <assemblyIdentity type="win32" name="{assembly_name}" version="{version}" processorArchitecture="*"/>
  <dependency>
    <dependentAssembly>
      <assemblyIdentity type="win32" name="Microsoft.Windows.Common-Controls" version="6.0.0.0" processorArchitecture="*" publicKeyToken="6595b64144ccf1df" language="*"/>
    </dependentAssembly>
  </dependency>
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1">
    <application>
      <!-- Windows 10 and Windows 11 -->
      <supportedOS Id="{{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}}"/>
    </application>
  </compatibility>
  <application xmlns="urn:schemas-microsoft-com:asm.v3">
    <windowsSettings>
      <dpiAware xmlns="http://schemas.microsoft.com/SMI/2005/WindowsSettings">true/pm</dpiAware>
      <dpiAwareness xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">PerMonitorV2, PerMonitor</dpiAwareness>
      <activeCodePage xmlns="http://schemas.microsoft.com/SMI/2019/WindowsSettings">UTF-8</activeCodePage>
    </windowsSettings>
  </application>
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="asInvoker" uiAccess="false"/>
      </requestedPrivileges>
    </security>
  </trustInfo>
</assembly>
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const ONE: Version = Version {
        major: 1,
        minor: 0,
        patch: 0,
    };

    #[test]
    fn manifests_need_a_four_part_version() {
        assert_eq!(ONE.four_part(), "1.0.0.0");
        let later = Version {
            major: 2,
            minor: 13,
            patch: 7,
        };
        assert_eq!(later.four_part(), "2.13.7.0");
    }

    #[test]
    fn the_manifest_names_the_program_and_its_settings() {
        let text = manifest("Downpour.App", ONE);
        assert!(text.contains(r#"name="Downpour.App" version="1.0.0.0""#));
        assert!(text.contains(">PerMonitorV2, PerMonitor<"));
        assert!(text.contains(r#"level="asInvoker""#));
        assert!(text.contains("Microsoft.Windows.Common-Controls"));
        assert!(text.contains("{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}"));
        assert!(text.contains(">UTF-8<"));
    }

    #[test]
    fn the_manifest_is_well_formed_xml() {
        // Every element opened is closed, in order; Windows refuses to start
        // a program whose manifest does not parse.
        let text = manifest("Downpour.App", ONE);
        let mut open: Vec<String> = Vec::new();
        for tag in text.split('<').skip(1) {
            let tag = tag.split('>').next().expect("a tag ends with >");
            if tag.starts_with('?') || tag.starts_with('!') || tag.ends_with('/') {
                continue;
            }
            let name = tag
                .trim_start_matches('/')
                .split_whitespace()
                .next()
                .unwrap();
            if tag.starts_with('/') {
                assert_eq!(open.pop().as_deref(), Some(name), "unbalanced </{name}>");
            } else {
                open.push(name.to_owned());
            }
        }
        assert!(open.is_empty(), "unclosed elements: {open:?}");
    }
}
