//! `.lnk` shortcuts, written through the Windows shell so they can carry
//! Downpour's AppUserModelID. With the ID on the shortcut, the taskbar
//! groups the running window with a pinned shortcut, and Windows knows the
//! app behind its notifications.

use std::path::Path;

/// One shortcut to write.
#[derive(Debug, PartialEq, Eq)]
pub struct ShortcutSpec<'a> {
    pub link: &'a Path,
    pub target: &'a Path,
    pub working_dir: Option<&'a Path>,
    pub description: &'a str,
    /// `None` for shortcuts to programs that never show a window of their
    /// own app (the uninstaller).
    pub app_user_model_id: Option<&'a str>,
}

#[cfg(windows)]
pub fn create(spec: &ShortcutSpec<'_>) -> windows::core::Result<()> {
    use windows::core::{Interface, HSTRING};
    use windows::Win32::Storage::EnhancedStorage::PKEY_AppUserModel_ID;
    use windows::Win32::System::Com::StructuredStorage::{
        PropVariantChangeType, PROPVARIANT, PVCHF_DEFAULT,
    };
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoUninitialize, IPersistFile, CLSCTX_INPROC_SERVER,
        COINIT_APARTMENTTHREADED,
    };
    use windows::Win32::System::Variant::VT_LPWSTR;
    use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;
    use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};

    /// Balances a successful `CoInitializeEx` on this thread.
    struct ComApartment;
    impl Drop for ComApartment {
        fn drop(&mut self) {
            // SAFETY: only created after `CoInitializeEx` succeeded here.
            unsafe { CoUninitialize() };
        }
    }

    // SAFETY: COM is initialized for this thread for the duration of the
    // call, and every interface is released before `CoUninitialize`.
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
        let _apartment = ComApartment;
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
        link.SetPath(&HSTRING::from(spec.target))?;
        if let Some(dir) = spec.working_dir {
            link.SetWorkingDirectory(&HSTRING::from(dir))?;
        }
        link.SetDescription(&HSTRING::from(spec.description))?;
        link.SetIconLocation(&HSTRING::from(spec.target), 0)?;
        if let Some(id) = spec.app_user_model_id {
            // The shell wants the ID as a wide string (VT_LPWSTR), not the
            // BSTR `PROPVARIANT::from` makes.
            let text = PROPVARIANT::from(id);
            let mut value = PROPVARIANT::default();
            PropVariantChangeType(&mut value, &text, PVCHF_DEFAULT, VT_LPWSTR)?;
            let store: IPropertyStore = link.cast()?;
            store.SetValue(&PKEY_AppUserModel_ID, &value)?;
            store.Commit()?;
        }
        let file: IPersistFile = link.cast()?;
        file.Save(&HSTRING::from(spec.link), true)?;
    }
    Ok(())
}
