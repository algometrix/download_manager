//! Embeds the Downpour icon, version details, and application manifest into
//! downpour-cli.exe.

fn main() {
    downpour_build::embed_windows_resources(downpour_build::Executable {
        file_name: "downpour-cli.exe",
        description: "Downpour command-line downloader",
        assembly_name: "Downpour.Cli",
    });
}
