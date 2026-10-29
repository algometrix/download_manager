use std::time::{Duration, Instant};

use eframe::egui::{self, vec2, Align2, Sense};

use super::icons::Icon;
use super::theme;
use super::widgets::elided;

const VISIBLE_FOR: Duration = Duration::from_secs(5);
const WIDTH: f32 = 340.0;
const BADGE_SIZE: f32 = 30.0;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    Success,
    Info,
    Error,
}

struct Toast {
    kind: ToastKind,
    title: String,
    body: String,
    shown_until: Instant,
}

/// Short-lived notifications in the bottom-right corner.
#[derive(Default)]
pub struct Toasts {
    items: Vec<Toast>,
}

impl Toasts {
    pub fn push(&mut self, kind: ToastKind, title: impl Into<String>, body: impl Into<String>) {
        self.items.push(Toast {
            kind,
            title: title.into(),
            body: body.into(),
            shown_until: Instant::now() + VISIBLE_FOR,
        });
    }

    pub fn show(&mut self, ctx: &egui::Context) {
        let now = Instant::now();
        self.items.retain(|toast| toast.shown_until > now);
        let Some(next_expiry) = self.items.iter().map(|toast| toast.shown_until).min() else {
            return;
        };
        ctx.request_repaint_after(next_expiry - now);

        egui::Area::new(egui::Id::new("downpour-toasts"))
            .anchor(Align2::RIGHT_BOTTOM, vec2(-20.0, -20.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                for toast in self.items.iter().rev() {
                    let (accent, icon) = match toast.kind {
                        ToastKind::Success => (theme::success(), Icon::Check),
                        ToastKind::Info => (theme::primary(), Icon::Download),
                        ToastKind::Error => (theme::danger(), Icon::Alert),
                    };
                    theme::card(ui, egui::Margin::same(14), |ui| {
                        ui.set_width(WIDTH);
                        ui.horizontal(|ui| {
                            let (badge, _) = ui
                                .allocate_exact_size(vec2(BADGE_SIZE, BADGE_SIZE), Sense::hover());
                            ui.painter().rect_filled(
                                badge,
                                theme::control_radius(badge.height()),
                                accent.gamma_multiply(0.12),
                            );
                            icon.paint(ui.painter(), badge.shrink(7.0), accent);
                            ui.vertical(|ui| {
                                let width = ui.available_width();
                                ui.label(elided(
                                    ui,
                                    toast.title.as_str(),
                                    theme::semibold(13.5),
                                    theme::ink(),
                                    width,
                                ));
                                if !toast.body.is_empty() {
                                    ui.add(
                                        egui::Label::new(
                                            egui::RichText::new(&toast.body)
                                                .font(theme::regular(12.5))
                                                .color(theme::ink_soft()),
                                        )
                                        .wrap(),
                                    );
                                }
                            });
                        });
                    });
                    ui.add_space(8.0);
                }
            });
    }
}
