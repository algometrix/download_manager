use downpour_core::{EngineSettings, Store, StoreError};
use serde::{Deserialize, Serialize};

const SETTINGS_FILE: &str = "settings.json";

/// What happens to a download the browser or the Add URL box hands over.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NewDownloadMode {
    /// Queue it at once; it starts as soon as its queue has a free slot.
    StartRightAway,
    /// Show a dialog to confirm the name, folder, quality, and queue first.
    AskFirst,
}

/// Which colors the window uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Appearance {
    /// Follow Windows' light or dark app setting, also when it changes.
    MatchSystem,
    Light,
    Dark,
}

/// The colors the window is drawn in; each comes in light and dark. The
/// order is `ALL`'s, and each theme's number is its place there.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum ColorTheme {
    /// The app icon's slate, coral, and white, with a sunny yellow: flat
    /// cards, pill controls, and an underlined sidebar choice.
    #[default]
    Downpour,
    /// Indigo with an orange accent.
    Lavender,
    /// Warm orange with a violet accent.
    Sunset,
    /// Navy slate with a coral accent.
    Slate,
    /// Teal with a pink accent.
    Ocean,
    /// Neutral grays with a violet accent, for a quieter window.
    Graphite,
    /// Soft cream, raised like pressed paper, with peach.
    Cream,
    /// Charcoal cards in a dark bezel, with amber, like a music player.
    Ember,
    /// Gray canvas, a dark sidebar, and bold coral and orange blocks.
    Coral,
    /// A bright violet sidebar, big bold titles, and a dark main button.
    Violet,
}

impl ColorTheme {
    pub const ALL: [ColorTheme; 10] = [
        ColorTheme::Downpour,
        ColorTheme::Lavender,
        ColorTheme::Sunset,
        ColorTheme::Slate,
        ColorTheme::Ocean,
        ColorTheme::Graphite,
        ColorTheme::Cream,
        ColorTheme::Ember,
        ColorTheme::Coral,
        ColorTheme::Violet,
    ];

    pub fn name(self) -> &'static str {
        match self {
            ColorTheme::Downpour => "Downpour",
            ColorTheme::Lavender => "Lavender",
            ColorTheme::Sunset => "Sunset",
            ColorTheme::Slate => "Slate",
            ColorTheme::Ocean => "Ocean",
            ColorTheme::Graphite => "Graphite",
            ColorTheme::Cream => "Cream",
            ColorTheme::Ember => "Ember",
            ColorTheme::Coral => "Coral",
            ColorTheme::Violet => "Violet",
        }
    }
}

/// What the window's close button does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloseAction {
    /// Hide to the tray; downloads keep running. Quit from the tray menu.
    HideToTray,
    Quit,
}

/// What minimizing the window does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MinimizeAction {
    HideToTray,
    /// Minimize to the taskbar like other windows.
    Taskbar,
}

/// Whether Windows starts Downpour when the user signs in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignInStart {
    /// Only when the user opens it.
    Manual,
    /// At sign-in, quietly in the tray.
    StartInTray,
}

/// Whether finished and failed downloads are announced with Windows
/// notifications.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DownloadNotifications {
    Show,
    Off,
}

/// Whether the PC may go to sleep while downloads run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SleepWhileDownloading {
    /// Keep the PC awake until the last running download stops.
    KeepAwake,
    /// Let Windows' power settings decide.
    Allow,
}

/// The main window's size in whole points, as last seen open normally.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowSize {
    pub width: u32,
    pub height: u32,
}

impl WindowSize {
    pub const DEFAULT: WindowSize = WindowSize {
        width: 1280,
        height: 820,
    };
    pub const MINIMUM: WindowSize = WindowSize {
        width: 1040,
        height: 640,
    };

    /// A size worth remembering, or `None` for what a minimized, hidden, or
    /// not yet laid out window reports (zero, or below the minimum). Saving
    /// those would open the next session with an unusable window.
    pub fn remembered(width: f32, height: f32) -> Option<WindowSize> {
        let fits = |value: f32, minimum: u32| value.is_finite() && value >= minimum as f32;
        (fits(width, Self::MINIMUM.width) && fits(height, Self::MINIMUM.height)).then(|| {
            WindowSize {
                width: width.round() as u32,
                height: height.round() as u32,
            }
        })
    }

    pub fn points(self) -> [f32; 2] {
        [self.width as f32, self.height as f32]
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    pub engine: EngineSettings,
    pub new_downloads: NewDownloadMode,
    pub appearance: Appearance,
    pub color_theme: ColorTheme,
    pub on_close: CloseAction,
    pub on_minimize: MinimizeAction,
    pub sign_in_start: SignInStart,
    pub notifications: DownloadNotifications,
    pub sleep: SleepWhileDownloading,
    /// `None` until the window has been open once.
    pub window_size: Option<WindowSize>,
    /// Replaced by `new_downloads`; read once so an older settings file
    /// keeps its choice.
    #[serde(skip_serializing)]
    confirm_browser_downloads: Option<bool>,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            engine: EngineSettings::default(),
            new_downloads: NewDownloadMode::AskFirst,
            appearance: Appearance::MatchSystem,
            color_theme: ColorTheme::default(),
            on_close: CloseAction::HideToTray,
            on_minimize: MinimizeAction::HideToTray,
            sign_in_start: SignInStart::Manual,
            notifications: DownloadNotifications::Show,
            sleep: SleepWhileDownloading::KeepAwake,
            window_size: None,
            confirm_browser_downloads: None,
        }
    }
}

impl AppSettings {
    pub fn load(store: &Store) -> Result<AppSettings, StoreError> {
        let mut settings: AppSettings = store.load(SETTINGS_FILE)?.unwrap_or_default();
        if let Some(confirm) = settings.confirm_browser_downloads.take() {
            settings.new_downloads = if confirm {
                NewDownloadMode::AskFirst
            } else {
                NewDownloadMode::StartRightAway
            };
        }
        Ok(AppSettings {
            engine: settings.engine.normalized(),
            ..settings
        })
    }

    pub fn save(&self, store: &Store) -> Result<(), StoreError> {
        store.save(SETTINGS_FILE, self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_real_window_sizes_are_remembered() {
        assert_eq!(
            WindowSize::remembered(1500.4, 900.6),
            Some(WindowSize {
                width: 1500,
                height: 901
            })
        );
        // A minimized or hidden window reports nothing useful.
        assert_eq!(WindowSize::remembered(0.0, 0.0), None);
        assert_eq!(WindowSize::remembered(160.0, 28.0), None);
        assert_eq!(WindowSize::remembered(f32::NAN, 900.0), None);
    }

    #[test]
    fn settings_saved_before_the_desktop_options_get_their_defaults() {
        let settings: AppSettings = serde_json::from_str(r#"{"on_close": "quit"}"#).unwrap();
        assert_eq!(settings.on_close, CloseAction::Quit);
        assert_eq!(settings.sign_in_start, SignInStart::Manual);
        assert_eq!(settings.notifications, DownloadNotifications::Show);
        assert_eq!(settings.sleep, SleepWhileDownloading::KeepAwake);
    }

    #[test]
    fn old_confirmation_setting_becomes_the_new_download_mode() {
        let dir = std::env::temp_dir().join(format!("downpour-settings-{}", std::process::id()));
        let store = Store::open(&dir).unwrap();
        std::fs::write(
            dir.join(SETTINGS_FILE),
            r#"{"confirm_browser_downloads": false}"#,
        )
        .unwrap();
        let settings = AppSettings::load(&store).unwrap();
        assert_eq!(settings.new_downloads, NewDownloadMode::StartRightAway);
        settings.save(&store).unwrap();
        let saved = std::fs::read_to_string(dir.join(SETTINGS_FILE)).unwrap();
        assert!(!saved.contains("confirm_browser_downloads"));
        assert!(saved.contains("start_right_away"));
    }

    #[test]
    fn new_installs_start_in_the_downpour_theme() {
        assert_eq!(AppSettings::default().color_theme, ColorTheme::Downpour);
        let unset: AppSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(unset.color_theme, ColorTheme::Downpour);
    }

    #[test]
    fn a_saved_theme_is_read_back_by_its_name() {
        let saved: AppSettings = serde_json::from_str(r#"{"color_theme": "violet"}"#).unwrap();
        assert_eq!(saved.color_theme, ColorTheme::Violet);
        let saved: AppSettings = serde_json::from_str(r#"{"color_theme": "downpour"}"#).unwrap();
        assert_eq!(saved.color_theme, ColorTheme::Downpour);
        let written = serde_json::to_string(&AppSettings::default()).unwrap();
        assert!(written.contains(r#""color_theme":"downpour""#), "{written}");
    }
}
