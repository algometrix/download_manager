//! Embeds the Downpour icon, version details, and application manifest into
//! downpour-host.exe.

fn main() {
    downpour_build::embed_windows_resources(downpour_build::Executable {
        file_name: "downpour-host.exe",
        description: "Downpour browser helper",
        assembly_name: "Downpour.Host",
    });
}
