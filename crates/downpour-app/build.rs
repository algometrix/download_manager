//! Embeds the Downpour icon, version details, and application manifest into
//! downpour.exe, so Explorer, the taskbar, and Start menu shortcuts show the
//! mark.

fn main() {
    downpour_build::embed_windows_resources(downpour_build::Executable {
        file_name: "downpour.exe",
        description: "Downpour",
        assembly_name: "Downpour.App",
    });
}
