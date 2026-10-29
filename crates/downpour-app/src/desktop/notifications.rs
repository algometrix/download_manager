//! Windows notifications when downloads finish or fail.
//!
//! Notifications are shown from their own thread, so a slow or broken
//! notification service never holds up the window. Failures are logged and
//! otherwise ignored: the in-app toasts still say the same thing.
//!
//! Windows shows Downpour's name and icon when the installer registered its
//! AppUserModelID (`downpour_protocol::desktop`). A click on a notification
//! that is still on screen brings the window forward. Clicking one later in
//! the notification center does nothing, because an unpackaged app would
//! need a COM activator registered for that.

use std::sync::mpsc;
use std::sync::Arc;

use downpour_core::DownloadStatus;

use crate::app_settings::DownloadNotifications;

/// More notifications than this at once are summed up, so a dropped
/// connection that fails twenty downloads shows one notification, not twenty.
const MAX_SEPARATE_NOTIFICATIONS: usize = 3;

/// How a download ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Completed,
    Failed { reason: String },
}

/// One finished or failed download to announce.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Announcement {
    pub file_name: String,
    pub outcome: Outcome,
}

impl Announcement {
    /// The announcement for a download that just reached `status`, if that
    /// status is worth a notification.
    pub fn for_status(file_name: &str, status: &DownloadStatus) -> Option<Announcement> {
        let outcome = match status {
            DownloadStatus::Completed => Outcome::Completed,
            DownloadStatus::Failed { reason } => Outcome::Failed {
                reason: reason.clone(),
            },
            _ => return None,
        };
        Some(Announcement {
            file_name: file_name.to_owned(),
            outcome,
        })
    }

    /// How this download is announced, in a Windows notification and in
    /// the app's own toast alike.
    pub fn text(&self) -> NotificationText {
        match &self.outcome {
            Outcome::Completed => NotificationText {
                title: "Download complete".to_owned(),
                body: self.file_name.clone(),
            },
            Outcome::Failed { reason } => NotificationText {
                title: format!("{} failed", self.file_name),
                body: reason.clone(),
            },
        }
    }
}

/// Whether the user is looking at Downpour right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppFocus {
    /// A Downpour window has the keyboard focus; the in-app toast is enough.
    Focused,
    /// Downpour is in the tray or behind other windows.
    Elsewhere,
}

pub fn should_notify(setting: DownloadNotifications, focus: AppFocus) -> bool {
    setting == DownloadNotifications::Show && focus == AppFocus::Elsewhere
}

/// The text of one Windows notification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotificationText {
    pub title: String,
    pub body: String,
}

/// What to show for the announcements of one frame.
pub fn notifications_for(batch: &[Announcement]) -> Vec<NotificationText> {
    if batch.len() <= MAX_SEPARATE_NOTIFICATIONS {
        return batch.iter().map(Announcement::text).collect();
    }
    let completed = batch
        .iter()
        .filter(|announcement| announcement.outcome == Outcome::Completed)
        .count();
    let failed = batch.len() - completed;
    let mut summary = Vec::new();
    if completed > 0 {
        summary.push(NotificationText {
            title: count_of(completed, "download complete", "downloads complete"),
            body: "Open Downpour to see them.".to_owned(),
        });
    }
    if failed > 0 {
        summary.push(NotificationText {
            title: count_of(failed, "download failed", "downloads failed"),
            body: "Open Downpour to see why and try again.".to_owned(),
        });
    }
    summary
}

fn count_of(count: usize, one: &str, many: &str) -> String {
    match count {
        1 => format!("1 {one}"),
        _ => format!("{count} {many}"),
    }
}

/// The notification as toast XML. File names and server messages are
/// untrusted, so all text is escaped.
pub fn toast_xml(text: &NotificationText) -> String {
    format!(
        "<toast><visual><binding template=\"ToastGeneric\"><text>{}</text><text>{}</text></binding></visual></toast>",
        escape_xml(&text.title),
        escape_xml(&text.body)
    )
}

fn escape_xml(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            // XML 1.0 has no form at all for most control characters.
            control if control.is_control() && !matches!(control, '\t' | '\n' | '\r') => {
                escaped.push(' ')
            }
            other => escaped.push(other),
        }
    }
    escaped
}

#[derive(Debug, thiserror::Error)]
pub enum NotificationError {
    #[error("start the notification thread: {0}")]
    Thread(std::io::Error),
    #[cfg(windows)]
    #[error("Windows notifications: {0}")]
    Windows(#[from] windows::core::Error),
}

/// Called when the user clicks a notification; runs on a Windows thread.
pub type OnClick = Arc<dyn Fn() + Send + Sync>;

/// Hands announcements to the notification thread, one frame's worth at a
/// time, so they are shown (or summed up) together.
pub struct Notifier {
    sender: mpsc::Sender<Vec<Announcement>>,
}

impl Notifier {
    pub fn start(on_click: OnClick) -> Result<Notifier, NotificationError> {
        let (sender, receiver) = mpsc::channel();
        std::thread::Builder::new()
            .name("downpour-notifications".into())
            .spawn(move || native::run(&receiver, &on_click))
            .map_err(NotificationError::Thread)?;
        Ok(Notifier { sender })
    }

    pub fn announce(&self, batch: Vec<Announcement>) {
        if batch.is_empty() {
            return;
        }
        if let Err(unsent) = self.sender.send(batch) {
            tracing::warn!(
                count = unsent.0.len(),
                "the notification thread has stopped; notifications dropped"
            );
        }
    }
}

#[cfg(windows)]
mod native {
    use std::collections::VecDeque;
    use std::sync::mpsc;

    use windows::core::{IInspectable, Ref, HSTRING};
    use windows::Data::Xml::Dom::XmlDocument;
    use windows::Foundation::TypedEventHandler;
    use windows::UI::Notifications::{ToastNotification, ToastNotificationManager, ToastNotifier};

    use super::{notifications_for, toast_xml, Announcement, NotificationError, OnClick};
    use crate::desktop::com::{Apartment, ComApartment};

    /// Notifications on screen keep their click handler only while their
    /// object lives, so the last few are kept.
    const KEPT_NOTIFICATIONS: usize = 8;

    /// Runs until the `Notifier` is dropped, then leaves COM after its
    /// objects are released.
    pub fn run(receiver: &mpsc::Receiver<Vec<Announcement>>, on_click: &OnClick) {
        // Declared first, so it drops last: after `kept` and `notifier`.
        let _apartment = match ComApartment::enter(Apartment::MultiThreaded) {
            Ok(apartment) => apartment,
            Err(error) => {
                tracing::warn!(%error, "no Windows notifications");
                return;
            }
        };
        let mut notifier: Option<ToastNotifier> = None;
        let mut kept: VecDeque<ToastNotification> = VecDeque::new();
        while let Ok(batch) = receiver.recv() {
            for text in notifications_for(&batch) {
                let shown = notifier_for(&mut notifier)
                    .and_then(|notifier| show(notifier, &toast_xml(&text), on_click));
                match shown {
                    Ok(toast) => {
                        if kept.len() == KEPT_NOTIFICATIONS {
                            kept.pop_front();
                        }
                        kept.push_back(toast);
                    }
                    Err(error) => {
                        tracing::warn!(%error, title = %text.title, "could not show a Windows notification");
                    }
                }
            }
        }
    }

    /// Created on first use and kept; a failed attempt is retried with the
    /// next notification.
    fn notifier_for(
        notifier: &mut Option<ToastNotifier>,
    ) -> Result<&ToastNotifier, NotificationError> {
        if notifier.is_none() {
            let id = HSTRING::from(downpour_protocol::desktop::APP_USER_MODEL_ID);
            *notifier = Some(ToastNotificationManager::CreateToastNotifierWithId(&id)?);
        }
        Ok(notifier.as_ref().expect("just created"))
    }

    fn show(
        notifier: &ToastNotifier,
        xml: &str,
        on_click: &OnClick,
    ) -> Result<ToastNotification, NotificationError> {
        let document = XmlDocument::new()?;
        document.LoadXml(&HSTRING::from(xml))?;
        let toast = ToastNotification::CreateToastNotification(&document)?;
        let on_click = OnClick::clone(on_click);
        toast.Activated(&TypedEventHandler::new(
            move |_: Ref<ToastNotification>, _: Ref<IInspectable>| {
                on_click();
                Ok(())
            },
        ))?;
        notifier.Show(&toast)?;
        Ok(toast)
    }
}

#[cfg(not(windows))]
mod native {
    use std::sync::mpsc;

    use super::{Announcement, OnClick};

    pub fn run(receiver: &mpsc::Receiver<Vec<Announcement>>, _on_click: &OnClick) {
        while let Ok(batch) = receiver.recv() {
            tracing::debug!(
                count = batch.len(),
                "no desktop notifications on this platform"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn completed(name: &str) -> Announcement {
        Announcement {
            file_name: name.to_owned(),
            outcome: Outcome::Completed,
        }
    }

    fn failed(name: &str) -> Announcement {
        Announcement {
            file_name: name.to_owned(),
            outcome: Outcome::Failed {
                reason: "the server closed the connection".to_owned(),
            },
        }
    }

    #[test]
    fn only_finished_and_failed_downloads_are_announced() {
        assert_eq!(
            Announcement::for_status("a.zip", &DownloadStatus::Completed),
            Some(completed("a.zip"))
        );
        assert!(matches!(
            Announcement::for_status(
                "a.zip",
                &DownloadStatus::Failed {
                    reason: "gone".into()
                }
            ),
            Some(Announcement {
                outcome: Outcome::Failed { .. },
                ..
            })
        ));
        assert_eq!(
            Announcement::for_status("a.zip", &DownloadStatus::Paused),
            None
        );
    }

    #[test]
    fn notifications_wait_while_the_user_is_in_the_app() {
        assert!(should_notify(
            DownloadNotifications::Show,
            AppFocus::Elsewhere
        ));
        assert!(!should_notify(
            DownloadNotifications::Show,
            AppFocus::Focused
        ));
        assert!(!should_notify(
            DownloadNotifications::Off,
            AppFocus::Elsewhere
        ));
    }

    #[test]
    fn a_few_downloads_get_one_notification_each() {
        let shown = notifications_for(&[completed("a.zip"), failed("b.iso")]);
        assert_eq!(
            shown,
            [
                NotificationText {
                    title: "Download complete".into(),
                    body: "a.zip".into()
                },
                NotificationText {
                    title: "b.iso failed".into(),
                    body: "the server closed the connection".into()
                },
            ]
        );
    }

    #[test]
    fn many_at_once_are_summed_up_by_outcome() {
        let batch = [completed("1"), completed("2"), failed("3"), completed("4")];
        let titles: Vec<String> = notifications_for(&batch)
            .into_iter()
            .map(|text| text.title)
            .collect();
        assert_eq!(titles, ["3 downloads complete", "1 download failed"]);
    }

    #[test]
    fn file_names_cannot_break_out_of_the_toast_xml() {
        let xml = toast_xml(&NotificationText {
            title: "Download complete".into(),
            body: "</text><image src=\"x\"/> & 'more'\u{1}".into(),
        });
        assert!(xml.contains(
            "<text>&lt;/text&gt;&lt;image src=&quot;x&quot;/&gt; &amp; &apos;more&apos; </text>"
        ));
        assert_eq!(xml.matches("<text>").count(), 2);
    }
}
